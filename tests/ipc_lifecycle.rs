use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};

use luatools_linux::services::protocol_service::ProtocolService;
use luatools_linux::services::steam_service::SteamService;

#[test]
fn test_single_instance_flock_mutual_exclusion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lock_path = tmp.path().join("test_instance.lock");

    // Process A acquires lock
    let file_a = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open file a");

    let fd_a = file_a.as_raw_fd();
    let ret_a = unsafe { libc::flock(fd_a, libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(ret_a, 0, "Process A must successfully acquire flock");

    // Process B attempts to acquire same lock
    let file_b = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open file b");

    let fd_b = file_b.as_raw_fd();
    let ret_b = unsafe { libc::flock(fd_b, libc::LOCK_EX | libc::LOCK_NB) };
    assert_ne!(
        ret_b, 0,
        "Process B must fail to acquire lock while held by Process A"
    );

    // Drop file A (releases flock)
    drop(file_a);

    // Process B attempts again
    let ret_b2 = unsafe { libc::flock(fd_b, libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(
        ret_b2, 0,
        "Process B must acquire lock once Process A released it"
    );
}

#[test]
fn test_cross_instance_unix_socket_ipc_protocol_forwarding() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock_path = tmp.path().join("test_ipc.sock");

    let listener = UnixListener::bind(&sock_path).expect("bind socket");
    let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));

    let server_handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept stream 1");
        let mut reader = BufReader::new(stream);
        let mut line1 = String::new();
        reader.read_line(&mut line1).expect("read line 1");

        let (stream2, _) = listener.accept().expect("accept stream 2");
        let mut reader2 = BufReader::new(stream2);
        let mut line2 = String::new();
        reader2.read_line(&mut line2).expect("read line 2");

        (line1, line2)
    });

    // Client sends URL
    {
        let mut client1 = UnixStream::connect(&sock_path).expect("connect client 1");
        client1
            .write_all(b"URL luatools://install/silent/1245620?source=morrenus\n")
            .expect("write url");
        client1.flush().expect("flush");
    }

    // Client sends FOCUS
    {
        let mut client2 = UnixStream::connect(&sock_path).expect("connect client 2");
        client2.write_all(b"FOCUS\n").expect("write focus");
        client2.flush().expect("flush");
    }

    let (recv_url, recv_focus) = server_handle.join().expect("join server");
    assert_eq!(
        recv_url.trim(),
        "URL luatools://install/silent/1245620?source=morrenus"
    );
    assert_eq!(recv_focus.trim(), "FOCUS");

    let raw_url = recv_url.trim().strip_prefix("URL ").unwrap();
    let action = ProtocolService::parse(raw_url).expect("parse action");
    assert_eq!(action.kind, "install");
    assert_eq!(action.app_id, 1245620);
    assert_eq!(action.source.as_deref(), Some("morrenus"));
    assert!(action.silent);
}

#[test]
fn test_steam_service_api() {
    let running = SteamService::is_steam_running();
    // Should be boolean without panicking (never terminate live host Steam processes in unit tests)
    let _ = running;

    // Relaunch command spec + detached spawn targeting an isolated mock steam.sh in temp directory
    let tmp = tempfile::tempdir().expect("tempdir");
    let mock_steam = tmp.path().join("steam.sh");
    std::fs::write(&mock_steam, "#!/bin/sh\nexit 0\n").expect("write mock steam.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&mock_steam).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&mock_steam, perms).unwrap();
    }

    let (prog, args, audit) =
        SteamService::build_relaunch_command_spec(Some(tmp.path()), tmp.path());
    assert_eq!(prog, mock_steam);
    let res = SteamService::spawn_detached_steam_and_verify(&prog, &args, audit.as_deref(), |_| {});
    assert!(
        res.is_ok(),
        "isolated spawn with mock steam executable should succeed: {:?}",
        res
    );
}

#[test]
fn test_bst_protocol_parsing_and_ipc() {
    let act = ProtocolService::parse("bst://1245620/token?source=hubcap").expect("parse bst");
    assert_eq!(act.kind, "install");
    assert_eq!(act.app_id, 1245620);
    assert_eq!(act.source.as_deref(), Some("hubcap"));
    assert!(!act.silent);

    let act_silent = ProtocolService::parse("bst://install/silent/730").expect("parse bst silent");
    assert_eq!(act_silent.kind, "install");
    assert_eq!(act_silent.app_id, 730);
    assert!(act_silent.silent);
    assert_eq!(act_silent.source.as_deref(), Some("bst"));
}

#[test]
fn test_steam_ui_language_detection_and_mapping() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let reg_vdf = tmp.path().join("registry.vdf");
    std::fs::write(
        &reg_vdf,
        "\"Registry\" { \"HKCU\" { \"Software\" { \"Valve\" { \"Steam\" { \"Language\" \"schinese\" } } } } }",
    )
    .expect("write reg vdf");

    let lang = SteamService::detect_steam_language(Some(tmp.path()));
    assert_eq!(lang, Some("schinese".to_string()));

    let code = SteamService::detect_steam_ui_language_code(Some(tmp.path()));
    assert_eq!(code, Some("zh".to_string()));

    assert_eq!(SteamService::steam_lang_to_ui_code("russian"), "ru");
    assert_eq!(SteamService::steam_lang_to_ui_code("english"), "en");
    assert_eq!(SteamService::steam_lang_to_ui_code("spanish"), "es");
    assert_eq!(SteamService::steam_lang_to_ui_code("german"), "de");
    assert_eq!(SteamService::steam_lang_to_ui_code("french"), "fr");
    assert_eq!(SteamService::steam_lang_to_ui_code("brazilian"), "pt");
    assert_eq!(SteamService::steam_lang_to_ui_code("japanese"), "ja");
    assert_eq!(SteamService::steam_lang_to_ui_code("italian"), "it");
    assert_eq!(SteamService::steam_lang_to_ui_code("koreana"), "ko");
}

#[test]
fn test_settings_autostart_logic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("settings.json");
    let svc = luatools_linux::services::settings_service::SettingsService::new(Some(cfg));
    assert!(!svc.settings.autostart);

    let desktop_p = luatools_linux::services::settings_service::SettingsService::autostart_desktop_path();
    assert!(desktop_p.is_some());
}
