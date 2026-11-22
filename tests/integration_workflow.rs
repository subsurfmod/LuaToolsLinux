//! End-to-end integration tests for LuaTools Linux v2.0.0.
//! Verifies multi-library Steam discovery, libraryfolders.vdf deduplication,
//! LuaInstaller + LuaVault variant switching + auto-update pin toggling,
//! SLSsteam lossless YAML sync, DonateKeys extraction, and FixesService backup/revert.

use std::fs;

use luatools_linux::models::lua_vault::{LuaVault, KIND_BUILD};
use luatools_linux::parsers::lua_parser::LuaFileParser;
use luatools_linux::parsers::steam_link::SteamLinkParser;
use luatools_linux::services::api_client::HubcapService;
use luatools_linux::services::donate_keys::DonateKeysService;
use luatools_linux::services::fixes_service::FixesService;
use luatools_linux::services::lua_installer::LuaInstaller;
use luatools_linux::services::protocol_service::{ProtocolAction, ProtocolService};
use luatools_linux::services::settings_service::SettingsService;
use luatools_linux::services::slssteam_service::SlssteamService;
use luatools_linux::services::steam_service::SteamService;

#[test]
fn full_workstation_end_to_end_pipeline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let steam_root = tmp.path().join("Steam");
    let ext_lib = tmp.path().join("GamesDrive/SteamLibrary");

    // 1. Multi-library Steam setup with primary path repeated in libraryfolders.vdf
    fs::create_dir_all(steam_root.join("steamapps/common/EldenRing")).unwrap();
    fs::create_dir_all(steam_root.join("config/stplug-in")).unwrap();
    fs::create_dir_all(ext_lib.join("steamapps/common/Cyberpunk 2077")).unwrap();

    let vdf = format!(
        "\"libraryfolders\"\n{{\n  \"0\" {{ \"path\" \"{}\" }}\n  \"1\" {{ \"path\" \"{}\" }}\n}}\n",
        steam_root.display(),
        ext_lib.display()
    );
    fs::write(steam_root.join("steamapps/libraryfolders.vdf"), vdf).unwrap();

    fs::write(
        steam_root.join("steamapps/appmanifest_1245620.acf"),
        "\"AppState\"\n{\n  \"appid\" \"1245620\"\n  \"name\" \"ELDEN RING\"\n  \"installdir\" \"EldenRing\"\n}\n",
    )
    .unwrap();
    fs::write(
        ext_lib.join("steamapps/appmanifest_1091500.acf"),
        "\"AppState\"\n{\n  \"appid\" \"1091500\"\n  \"name\" \"Cyberpunk 2077\"\n  \"installdir\" \"Cyberpunk 2077\"\n}\n",
    )
    .unwrap();

    let mut steam_svc = SteamService::new();
    let inst = steam_svc
        .detect(Some(&steam_root.to_string_lossy()))
        .expect("detect steam");
    assert!(steam_svc.is_available());
    assert_eq!(
        inst.libraries.len(),
        2,
        "Primary Steam root must not be duplicated when listed in libraryfolders.vdf"
    );
    assert_eq!(steam_svc.scan_all_games(), vec![1091500, 1245620]);
    assert_eq!(
        steam_svc.get_game_name(1091500).as_deref(),
        Some("Cyberpunk 2077")
    );
    assert!(steam_svc
        .find_install_dir(1245620)
        .unwrap()
        .ends_with("EldenRing"));

    // 2. Settings + SLSsteam + LuaVault + LuaInstaller (auto-update vs build lock)
    let mut settings_svc = SettingsService::new(Some(tmp.path().join("settings.json")));
    settings_svc.settings.auto_update_apps = true;
    assert!(settings_svc.save());

    let mut sls_svc = SlssteamService::new(Some(tmp.path().join("sls/config.yaml")));
    let vault = LuaVault::new(Some(inst.clone()), Some(tmp.path().join("luavault")));
    let installer = LuaInstaller::new(inst.clone(), vault.clone());

    let key_hex = "ab".repeat(32);
    let lua_v1 = format!(
        "addappid(1245620)\naddappid(1245621, 1, \"{}\")\naddappid(2778580) -- Shadow of the Erdtree\nsetManifestid(1245621, \"1111111111111111111\", 52428800)\naddtoken(1245620, \"tok_elden_999\")\n",
        key_hex
    );

    // With auto_update_apps = true and no build_id, setManifestid should be commented out
    let r1 = installer.install_lua(
        1245620,
        &lua_v1,
        false,
        None,
        Some("morrenus"),
        Some(&mut settings_svc),
        Some(&mut sls_svc),
    );
    assert!(r1.ok && r1.lua_installed);
    let live_after_r1 = installer.read_lua_text(1245620).unwrap();
    let parsed_r1 = LuaFileParser::parse_text(&live_after_r1, 1245620);
    assert!(parsed_r1.active_pins.is_empty());
    assert_eq!(
        parsed_r1.commented_pins.get(&1245621).map(String::as_str),
        Some("1111111111111111111")
    );

    // SLSsteam sync verification: key stays in DepotKeys, token in AppTokens, DLC in DlcData
    assert_eq!(sls_svc.get_fake_app_id(1245620), Some(480));
    assert_eq!(sls_svc.get_token(1245620), Some("tok_elden_999"));
    assert_eq!(
        sls_svc.config.depot_keys.get(&1245621).map(String::as_str),
        Some(key_hex.as_str())
    );
    assert_eq!(
        sls_svc.config.dlc_data[&1245620][&2778580],
        "Shadow of the Erdtree"
    );
    assert!(sls_svc.config.additional_apps.contains(&1245620));

    // Install a build-locked variant (must preserve active setManifestid pins even when auto_update_apps=true)
    let lua_build = format!(
        "addappid(1245620)\naddappid(1245621, 1, \"{}\")\nsetManifestid(1245621, \"2222222222222222222\", 52428800)\n",
        key_hex
    );
    let r2 = installer.install_lua(
        1245620,
        &lua_build,
        true,
        Some("14890211".into()),
        Some("build-downgrade"),
        Some(&mut settings_svc),
        Some(&mut sls_svc),
    );
    assert!(r2.ok);
    let vars = vault.get_variants(1245620);
    assert_eq!(vars.len(), 2);
    let build_var = vars.iter().find(|v| v.kind == KIND_BUILD).unwrap();
    assert!(build_var.is_pinned());

    // Switch back to the first variant and verify active hash updates
    let default_var = vars.iter().find(|v| v.kind != KIND_BUILD).unwrap();
    assert!(vault.apply(1245620, &default_var.hash));
    assert_eq!(
        vault.get_active_hash(1245620).as_deref(),
        Some(default_var.hash.as_str())
    );

    // 3. Steam Link & Protocol & Hubcap validators
    assert_eq!(SteamLinkParser::app_id_from("Warhammer 40000"), None);
    assert_eq!(
        SteamLinkParser::app_id_from("https://store.steampowered.com/app/1245620/ELDEN_RING/"),
        Some(1245620)
    );
    assert_eq!(
        ProtocolService::parse("luatools://install/1245620?source=hubcap"),
        Some(ProtocolAction {
            kind: "install".into(),
            app_id: 1245620,
            source: Some("hubcap".into())
        })
    );
    let valid_hc = format!("smm_{}", "0123456789abcdef".repeat(6));
    assert!(HubcapService::is_valid_key_format(&valid_hc));
    assert!(!HubcapService::is_valid_key_format("smm_INVALID"));

    // 4. DonateKeys VDF key extraction
    let cfg_vdf = steam_root.join("config/config.vdf");
    fs::write(
        &cfg_vdf,
        format!(
            "\"InstallConfigStore\" {{ \"Software\" {{ \"Valve\" {{ \"Steam\" {{ \"depots\" {{ \"1245621\" {{ \"DecryptionKey\" \"{}\" }} }} }} }} }} }}",
            "EF".repeat(32)
        ),
    )
    .unwrap();
    let harvested = DonateKeysService::extract_keys_from_vdf(&cfg_vdf);
    assert_eq!(
        harvested.get("1245621").map(String::as_str),
        Some("ef".repeat(32).as_str())
    );

    // 5. FixesService apply + tamper check + clean revert
    let game_dir = steam_svc.find_install_dir(1245620).unwrap();
    fs::write(game_dir.join("eldenring.exe"), b"ORIG_EXE").unwrap();
    let patch_dir = tmp.path().join("patch_extract");
    fs::create_dir_all(&patch_dir).unwrap();
    fs::write(patch_dir.join("eldenring.exe"), b"FIXED_EXE").unwrap();
    fs::write(patch_dir.join("winmm.dll"), b"HOOK_DLL").unwrap();

    let fx_res =
        FixesService::apply_extracted_with_backup(1245620, "online-fix", &patch_dir, &game_dir);
    assert!(fx_res.ok);
    assert!(FixesService::is_fix_applied(&game_dir, "online-fix"));

    let unfix_res = FixesService::unfix_game(&game_dir, "online-fix");
    assert!(unfix_res.ok);
    assert_eq!(
        fs::read(game_dir.join("eldenring.exe")).unwrap(),
        b"ORIG_EXE"
    );
    assert!(!game_dir.join("winmm.dll").exists());
}
