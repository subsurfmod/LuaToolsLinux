//! Main `LuaToolsApp` state coordinator & Papercut Monotone-Dark window shell (`eframe::App`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde::{Deserialize, Serialize};

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Key,
    KeyboardShortcut, Layout, Margin, Modifiers, Pos2, RichText, Sense, Stroke, TextEdit,
    TextureHandle, TextureOptions, Vec2,
};
use egui_extras::{Size, StripBuilder};

use crate::models::lua_vault::{LuaVariant, LuaVault};
use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::parsers::steam_link::SteamLinkParser;
use crate::services::analytics_service::AnalyticsService;
use crate::services::api_client::{
    DRMFixFixItem, DRMFixGameListing, DRMFixTag, DlcInfo, HubcapService, LuaToolsApiClient,
    SteamFeaturedItem, SteamSearchItem,
};
use crate::services::asset_hash::AssetHashService;
use crate::services::auth_service::AuthService;
use crate::services::caches::{
    CoverCache, GameDetails, HardwareAppIdService, SteamAppInfoCache, SteamAppListCache,
};
use crate::services::cef_bridge::CefBridgeService;
use crate::services::cloud_redirect_service::CloudRedirectService;
use crate::services::depot_downloader::DepotDownloaderService;
use crate::services::depot_info::{AppDepotInfo, ContentDepot, SteamDepotInfo};
use crate::services::donate_keys::DonateKeysService;
use crate::services::download_queue::DownloadQueue;
use crate::services::external_tools::{ExternalToolsManager, ToolStatusInfo};
use crate::services::fixes_service::FixesService;
use crate::services::game_process_injection_service::GameProcessInjectionService;
use crate::services::hot_reload_service::{FileChangeType, HotReloadService};
use crate::services::http_server::{HttpServerService, RpcUiEvent};
use crate::services::lua_installer::LuaInstaller;
use crate::services::manifest_binding_service::ManifestBindingService;
use crate::services::millennium_service::MillenniumService;
use crate::services::play_unowned_service::PlayUnownedService;
use crate::services::protocol_service::ProtocolService;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
use crate::services::steamless_service::SteamlessService;
use crate::services::steam_auto_crack_service::SteamAutoCrackService;
use crate::services::steam_bridge_service::SteamBridgeService;
use crate::services::steam_library_service::SteamLibraryService;
use crate::services::steam_service::SteamService;
use crate::services::applied_fix_index::AppliedFixIndexService;
use crate::services::telemetry_service::{TelemetryEvent, TelemetryService};
use crate::services::tray_service::TrayService;
use crate::services::update_service::UpdateService;
use crate::services::wired_service::WiredService;
use crate::ui::state::{
    AcquireTab, ActiveView, AddByAppIdModal, AppMessage, DepotDetailModal, DepotPickRow,
    DepotsSubTab, GameDetailModal, GuideTopic, IntegrationModal, LaunchOptionsModal, LibraryRow,
    QaCaptureState, SettingsSubTab, TextPromptModal, ToastKind, ToastNotification,
    UpdateBannerState,
};
use crate::ui::theme::{
    apply_papercut_monotone_theme, display_font, draw_rem_medallion, nav_rail_button,
    primary_button, semibold_font, Palette,
};
use crate::VERSION;

#[derive(Debug, Clone, Default)]
pub struct OverwriteDiffModal {
    pub open: bool,
    pub app_id: u32,
    pub source_label: String,
    pub old_content: String,
    pub new_content: String,
    pub added_lines: Vec<String>,
    pub removed_lines: Vec<String>,
    pub pending_package_path: Option<PathBuf>,
    pub cleanup_parent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadHistoryEntry {
    pub app_id: u32,
    pub label: String,
    pub source: String,
    pub status: String,
    pub timestamp: String,
}

pub struct LuaToolsApp {
    // Core Services
    pub settings_service: SettingsService,
    pub auth_service: AuthService,
    pub steam_service: SteamService,
    pub steam_bridge_service: SteamBridgeService,
    pub slssteam_service: SlssteamService,
    pub millennium_service: MillenniumService,
    pub api_client: LuaToolsApiClient,
    pub hubcap_service: HubcapService,
    pub app_info_cache: SteamAppInfoCache,
    pub applist_cache: SteamAppListCache,
    pub cover_cache: CoverCache,
    pub hardware_service: HardwareAppIdService,
    pub depot_info_service: SteamDepotInfo,
    pub depot_downloader: DepotDownloaderService,
    pub download_queue: Arc<DownloadQueue>,
    pub lua_vault: LuaVault,
    pub lua_installer: Option<LuaInstaller>,
    pub play_unowned_service: PlayUnownedService,
    pub steam_library_service: SteamLibraryService,
    pub manifest_binding_service: ManifestBindingService,
    pub hot_reload_service: HotReloadService,
    pub cloud_redirect_service: CloudRedirectService,
    pub game_injection_service: GameProcessInjectionService,
    pub wired_service: WiredService,
    pub steam_auto_crack: SteamAutoCrackService,
    pub cef_bridge: CefBridgeService,
    pub http_server: Option<HttpServerService>,
    pub tools_manager: ExternalToolsManager,
    pub telemetry_service: TelemetryService,
    pub asset_hash_service: AssetHashService,
    pub applied_fix_index: AppliedFixIndexService,
    pub tray_service: TrayService,
    pub tray_locked: bool,
    pub quit_requested: bool,
    pub prev_viewport_minimized: bool,
    pub suppress_minimize_hook_frames: u8,
    pub tray_hide_notified: bool,

    // Async Channels
    pub egui_ctx: Option<egui::Context>,
    pub msg_tx: Sender<AppMessage>,
    pub msg_rx: Receiver<AppMessage>,
    pub rpc_tx: Sender<RpcUiEvent>,
    pub rpc_rx: Receiver<RpcUiEvent>,

    // Shell State
    pub active_view: ActiveView,
    pub command_input: String,
    pub focus_command_next: bool,
    pub status_text: String,
    pub active_toast: Option<ToastNotification>,
    pub millennium_connected: bool,
    pub cover_textures: HashMap<u32, TextureHandle>,
    pub cover_lru: VecDeque<u32>,
    pub cover_in_flight: HashSet<u32>,
    pub cover_known_urls: HashMap<u32, String>,
    pub cover_retry_urls: HashMap<u32, String>,
    pub cover_failed_at: HashMap<u32, std::time::Instant>,
    pub recent_installed: Vec<(u32, PathBuf)>,
    pub featured_requested: bool,
    pub fixes_requested: bool,
    pub steam_sls_installed: bool,
    pub cached_tool_statuses: HashMap<&'static str, ToolStatusInfo>,
    pub cached_telemetry_events: Vec<TelemetryEvent>,
    pub cached_telemetry_count: usize,
    pub telemetry_cache_dirty: bool,

    // Discover (Acquire) View State
    pub acquire_search_query: String,
    pub acquire_tab: AcquireTab,
    pub featured_top: Vec<SteamFeaturedItem>,
    pub featured_new: Vec<SteamFeaturedItem>,
    pub search_results: Vec<SteamSearchItem>,
    pub acquire_selected_app: Option<u32>,
    pub acquire_details: Option<GameDetails>,
    pub acquire_sources: BTreeMap<String, String>,
    pub acquire_selected_source: Option<String>,
    pub acquire_dlc_info: Option<DlcInfo>,
    pub acquire_busy: bool,
    pub acquire_progress: f32,
    pub acquire_status: String,

    // Library View State
    pub library_rows: Vec<LibraryRow>,
    pub library_filter_mode: usize,
    pub library_search: String,
    pub library_selected_ids: HashSet<u32>,

    // Depots & Vault State
    pub depots_app_id: Option<u32>,
    pub depots_jump_input: String,
    pub depots_subtab: DepotsSubTab,
    pub depots_lua: Option<LuaContents>,
    pub depots_info: Option<AppDepotInfo>,
    pub depots_variants: Vec<LuaVariant>,
    pub depots_selected_variant_hash: Option<String>,
    pub depots_editor_text: String,
    pub depots_editing_live: bool,
    pub depots_picks: Vec<DepotPickRow>,
    pub depots_dest_dir: String,
    pub depots_downloading: bool,
    pub depots_dl_pct: f32,
    pub depots_dl_status: String,
    pub binding_depot_input: String,
    pub binding_gid_input: String,
    pub binding_size_input: String,

    // Fixes View State
    pub fixes_listings: Vec<DRMFixGameListing>,
    pub fixes_tags: Vec<DRMFixTag>,
    pub fixes_selected_tag: Option<String>,
    pub fixes_search: String,
    pub fixes_my_games_only: bool,
    pub fixes_current_app: Option<u32>,
    pub fixes_items: Vec<DRMFixFixItem>,
    pub fixes_selected_row: Option<usize>,
    pub fixes_busy: bool,
    pub fixes_progress: f32,

    // Toolchain & Settings State
    pub settings_subtab: SettingsSubTab,
    pub tool_busy: HashMap<String, bool>,
    pub tool_progress: HashMap<String, f32>,
    pub settings_show_hubcap: bool,
    pub settings_hubcap_status: String,
    pub settings_usage_text: String,
    pub settings_millennium_status: String,
    pub settings_update_status: String,
    pub settings_telemetry_status: String,
    pub wired_busy: bool,
    pub wired_progress: f32,
    pub wired_status_text: String,
    pub bridge_system_preload: bool,
    pub bridge_busy: bool,
    pub bridge_status_msg: String,
    pub cloud_app_input: String,
    pub update_banner: UpdateBannerState,
    pub guide_topic: GuideTopic,
    pub guide_textures: HashMap<&'static str, TextureHandle>,
    pub guide_zoom_screenshot: Option<(&'static str, &'static str)>,
    pub last_hot_reload_key: Option<String>,
    pub first_run_quick_setup: bool,
    pub overwrite_diff_modal: OverwriteDiffModal,
    pub download_history: VecDeque<DownloadHistoryEntry>,
    pub download_history_path: PathBuf,
    pub pics_drift_checked: bool,

    // Modals & QA
    pub add_appid_modal: AddByAppIdModal,
    pub game_detail_modal: GameDetailModal,
    pub integration_modal: IntegrationModal,
    pub launch_options_modal: LaunchOptionsModal,
    pub depot_detail_modal: DepotDetailModal,
    pub text_prompt_modal: TextPromptModal,
    pub about_modal_open: bool,
    pub telemetry_modal_open: bool,
    pub qa_capture: Option<QaCaptureState>,
}

impl LuaToolsApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial_url: Option<String>,
        qa_screenshots_dir: Option<PathBuf>,
    ) -> Self {
        Self::new_with_tray_hint(cc, initial_url, qa_screenshots_dir, false)
    }

    pub fn new_with_tray_hint(
        cc: &eframe::CreationContext<'_>,
        initial_url: Option<String>,
        qa_screenshots_dir: Option<PathBuf>,
        force_tray: bool,
    ) -> Self {
        apply_papercut_monotone_theme(&cc.egui_ctx);

        let mut settings_service = SettingsService::default();
        if settings_service.settings.telemetry_enabled
            && settings_service
                .settings
                .telemetry_install_id
                .trim()
                .is_empty()
        {
            settings_service.settings.telemetry_install_id =
                TelemetryService::generate_install_id();
            let _ = settings_service.save();
        }
        let telemetry_service = TelemetryService::new(
            None,
            settings_service.settings.telemetry_enabled,
            settings_service.settings.telemetry_endpoint_url.clone(),
            settings_service.settings.telemetry_install_id.clone(),
        );
        telemetry_service.install_panic_hook();
        let auth_service = AuthService::default();
        auth_service.initialize();

        let steam_service = SteamService::new();
        let steam_bridge_service = SteamBridgeService::new();
        let slssteam_service = SlssteamService::default();
        let millennium_service = MillenniumService::default();
        let api_client = LuaToolsApiClient::new(auth_service.clone());
        let hubcap_service = HubcapService::new();

        let app_info_cache = SteamAppInfoCache::default();
        let applist_cache = SteamAppListCache::default();
        let cover_cache = CoverCache::default();
        let hardware_service = HardwareAppIdService::default();
        let depot_info_service = SteamDepotInfo::default();
        let depot_downloader = DepotDownloaderService::default();
        let lua_vault = LuaVault::new(None, None);
        let cef_bridge = CefBridgeService::new(None, settings_service.settings.http_server_port);
        let tools_manager = ExternalToolsManager::default();
        let asset_hash_service = AssetHashService::new();

        let (msg_tx, msg_rx) = mpsc::channel();
        let (rpc_tx, rpc_rx) = mpsc::channel();

        let default_dest = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("LuaTools Depots")
            .to_string_lossy()
            .to_string();

        let is_qa = qa_screenshots_dir.is_some();
        let qa_capture = qa_screenshots_dir.map(|out_dir| {
            let _ = fs::create_dir_all(&out_dir);
            QaCaptureState {
                out_dir,
                step: 0,
                frames_waited: 0,
                pending_save: None,
            }
        });

        let steam_installation = steam_service.installation.clone();
        let steam_auto_crack = SteamAutoCrackService::new(tools_manager.clone());
        let ctx_for_hr = cc.egui_ctx.clone();
        let hot_reload_service = HotReloadService::new().on_change(move |_| {
            ctx_for_hr.request_repaint();
        });
        let download_queue = Arc::new(DownloadQueue::default());
        download_queue.start_worker();
        let sys_preload = steam_bridge_service.status().system_preload_enabled;
        let mut app = Self {
            settings_service,
            auth_service,
            steam_service,
            steam_bridge_service,
            slssteam_service,
            millennium_service,
            api_client,
            hubcap_service,
            app_info_cache,
            applist_cache,
            cover_cache,
            hardware_service,
            depot_info_service,
            depot_downloader,
            download_queue,
            lua_vault,
            lua_installer: None,
            play_unowned_service: PlayUnownedService::new(steam_installation.clone()),
            steam_library_service: SteamLibraryService::new(steam_installation),
            manifest_binding_service: ManifestBindingService::default(),
            hot_reload_service,
            cloud_redirect_service: CloudRedirectService::default(),
            game_injection_service: GameProcessInjectionService::default(),
            wired_service: WiredService::new(None),
            steam_auto_crack,
            cef_bridge,
            http_server: None,
            tools_manager,
            telemetry_service,
            asset_hash_service,
            applied_fix_index: AppliedFixIndexService::new(None),
            tray_service: TrayService::default(),
            tray_locked: force_tray,
            quit_requested: false,
            prev_viewport_minimized: false,
            suppress_minimize_hook_frames: 0,
            tray_hide_notified: false,
            egui_ctx: Some(cc.egui_ctx.clone()),
            msg_tx,
            msg_rx,
            rpc_tx,
            rpc_rx,
            active_view: ActiveView::Overview,
            command_input: String::new(),
            focus_command_next: false,
            status_text: "Ready — drop .lua, .manifest, or .zip files anywhere to install".into(),
            active_toast: None,
            millennium_connected: false,
            cover_textures: HashMap::new(),
            cover_lru: VecDeque::new(),
            cover_in_flight: HashSet::new(),
            cover_known_urls: HashMap::new(),
            cover_retry_urls: HashMap::new(),
            cover_failed_at: HashMap::new(),
            recent_installed: Vec::new(),
            featured_requested: false,
            fixes_requested: false,
            steam_sls_installed: false,
            cached_tool_statuses: HashMap::new(),
            cached_telemetry_events: Vec::new(),
            cached_telemetry_count: 0,
            telemetry_cache_dirty: true,
            acquire_search_query: String::new(),
            acquire_tab: AcquireTab::TopSellers,
            featured_top: Vec::new(),
            featured_new: Vec::new(),
            search_results: Vec::new(),
            acquire_selected_app: None,
            acquire_details: None,
            acquire_sources: BTreeMap::new(),
            acquire_selected_source: None,
            acquire_dlc_info: None,
            acquire_busy: false,
            acquire_progress: 0.0,
            acquire_status: "Ready".into(),
            library_rows: Vec::new(),
            library_filter_mode: 0,
            library_search: String::new(),
            library_selected_ids: HashSet::new(),
            depots_app_id: None,
            depots_jump_input: String::new(),
            depots_subtab: DepotsSubTab::Declarations,
            depots_lua: None,
            depots_info: None,
            depots_variants: Vec::new(),
            depots_selected_variant_hash: None,
            depots_editor_text: String::new(),
            depots_editing_live: false,
            depots_picks: Vec::new(),
            depots_dest_dir: default_dest,
            depots_downloading: false,
            depots_dl_pct: 0.0,
            depots_dl_status: String::new(),
            binding_depot_input: String::new(),
            binding_gid_input: String::new(),
            binding_size_input: String::new(),
            fixes_listings: Vec::new(),
            fixes_tags: Vec::new(),
            fixes_selected_tag: None,
            fixes_search: String::new(),
            fixes_my_games_only: false,
            fixes_current_app: None,
            fixes_items: Vec::new(),
            fixes_selected_row: None,
            fixes_busy: false,
            fixes_progress: 0.0,
            settings_subtab: SettingsSubTab::General,
            tool_busy: HashMap::new(),
            tool_progress: HashMap::new(),
            settings_show_hubcap: false,
            settings_hubcap_status: String::new(),
            settings_usage_text: String::new(),
            settings_millennium_status:
                "Optional: Click 'Check Millennium Status' if you use Millennium Steam themes"
                    .into(),
            settings_update_status: "Checked automatically once on startup".into(),
            settings_telemetry_status: String::new(),
            wired_busy: false,
            wired_progress: 0.0,
            wired_status_text: String::new(),
            bridge_system_preload: sys_preload,
            bridge_busy: false,
            bridge_status_msg: String::new(),
            cloud_app_input: String::new(),
            update_banner: UpdateBannerState::Hidden,
            guide_topic: GuideTopic::QuickStart,
            guide_textures: HashMap::new(),
            guide_zoom_screenshot: None,
            last_hot_reload_key: None,
            first_run_quick_setup: true,
            overwrite_diff_modal: OverwriteDiffModal::default(),
            download_history: Self::load_download_history_from(
                &crate::models::config::default_luatools_data_dir().join("download_history.json"),
            ),
            download_history_path: crate::models::config::default_luatools_data_dir()
                .join("download_history.json"),
            pics_drift_checked: false,
            add_appid_modal: AddByAppIdModal::default(),
            game_detail_modal: GameDetailModal::default(),
            integration_modal: IntegrationModal::default(),
            launch_options_modal: LaunchOptionsModal::default(),
            depot_detail_modal: DepotDetailModal::default(),
            text_prompt_modal: TextPromptModal::default(),
            about_modal_open: false,
            telemetry_modal_open: false,
            qa_capture,
        };

        app.detect_steam_and_sync();
        if is_qa {
            app.populate_qa_demo_data();
        } else {
            app.telemetry_service.record(
                "info",
                "startup",
                "app_start",
                "LuaTools Linux desktop session initialized",
            );
            {
                let consent_seen = app.settings_service.settings.telemetry_consent_seen;
                let telemetry_on = app.settings_service.settings.telemetry_enabled;
                if consent_seen && telemetry_on {
                    thread::spawn(move || {
                        let _ = AnalyticsService::new()
                            .track_app_launch_gated(consent_seen, telemetry_on);
                    });
                }
            }
            if app.steam_service.is_available() {
                let _ = app.hot_reload_service.start();
                // Run depot cache migration (LT-34)
                crate::services::depot_cache_migration::run_depot_cache_migration(&app.steam_service);
            }
            // Wire system tray callbacks; only load GTK3/AppIndicator3 (~25 MB) when tray behavior is enabled
            app.tray_service.set_app_message_sender(app.msg_tx.clone());
            app.tray_service.set_egui_context(cc.egui_ctx.clone());
            let is_silent = initial_url
                .as_ref()
                .and_then(|u| ProtocolService::parse(u))
                .map(|a| a.silent)
                .unwrap_or(false);
            if force_tray
                || is_silent
                || app.settings_service.settings.close_to_tray
                || app.settings_service.settings.minimize_to_tray
            {
                let _ = app.tray_service.initialize();
            }

            if app.settings_service.settings.check_updates_on_startup {
                app.check_app_update_once_async(&cc.egui_ctx);
            }
            if app.auth_service.is_signed_in() {
                app.fetch_usage_async(&cc.egui_ctx);
            }
            if let Some(url) = initial_url {
                app.handle_protocol_url(&url, &cc.egui_ctx);
            }
        }

        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }

        app
    }

    /// Headless constructor used by offline visual & layout QA tests (uses `temp_dir` paths only).
    pub fn new_headless_demo(ctx: &egui::Context, scratch_dir: &Path) -> Self {
        apply_papercut_monotone_theme(ctx);
        let _ = fs::create_dir_all(scratch_dir);
        let mock_steam_root = scratch_dir.join("Steam");
        let _ = fs::create_dir_all(mock_steam_root.join("config/stplug-in"));
        let _ = fs::create_dir_all(mock_steam_root.join("steamapps/common"));
        let _ = fs::write(mock_steam_root.join("steam.sh"), "#!/bin/sh\n");
        let mock_inst = crate::models::steam::SteamInstallation {
            root: mock_steam_root.clone(),
            libraries: vec![
                crate::models::steam::SteamLibrary::new(mock_steam_root.clone()),
                crate::models::steam::SteamLibrary::new(scratch_dir.join("SteamGamesNVMe")),
            ],
            config_path: mock_steam_root.join("config"),
            stplug_in_path: mock_steam_root.join("config/stplug-in"),
        };

        let settings_service = SettingsService::new(Some(scratch_dir.join("settings.json")));
        let auth_service = AuthService::new(Some(scratch_dir.join("auth.json")));
        let mut steam_service = SteamService::new();
        steam_service.installation = Some(mock_inst.clone());
        let steam_bridge_service =
            SteamBridgeService::with_paths(Some(mock_inst), Some(scratch_dir.join("bridge")));
        let slssteam_service = SlssteamService::new(Some(scratch_dir.join("sls.yaml")));
        let millennium_service = MillenniumService::default();
        let api_client = LuaToolsApiClient::new(auth_service.clone());
        let hubcap_service = HubcapService::new();
        let app_info_cache = SteamAppInfoCache::new(Some(scratch_dir.join("appinfo")));
        let applist_cache = SteamAppListCache::new(Some(scratch_dir.join("applist.json")));
        let cover_cache = CoverCache::new(Some(scratch_dir.join("covers")));
        let hardware_service = HardwareAppIdService::new(Some(scratch_dir.join("hw.json")));
        let depot_info_service = SteamDepotInfo::new(Some(scratch_dir.join("depots")));
        let depot_downloader = DepotDownloaderService::new(Some(scratch_dir.join("dd")));
        let lua_vault = LuaVault::new(None, Some(scratch_dir.join("vault")));
        let cef_bridge = CefBridgeService::new(None, 6767);
        let tools_manager = ExternalToolsManager::new(Some(scratch_dir.join("tools")));
        let telemetry_service = TelemetryService::new(
            Some(scratch_dir.join("telemetry")),
            true,
            String::new(),
            "0102030405060708".into(),
        );
        let tray_service = TrayService::default();
        let (msg_tx, msg_rx) = mpsc::channel();
        let (rpc_tx, rpc_rx) = mpsc::channel();

        let steam_installation = steam_service.installation.clone();
        let steam_auto_crack = SteamAutoCrackService::new(tools_manager.clone());
        let download_queue = Arc::new(DownloadQueue::new(Some(scratch_dir.join("downloads"))));
        download_queue.start_worker();
        let mut app = Self {
            settings_service,
            auth_service,
            steam_service,
            steam_bridge_service,
            slssteam_service,
            millennium_service,
            api_client,
            hubcap_service,
            app_info_cache,
            applist_cache,
            cover_cache,
            hardware_service,
            depot_info_service,
            depot_downloader,
            download_queue,
            lua_vault,
            lua_installer: None,
            play_unowned_service: PlayUnownedService::new(steam_installation.clone()),
            steam_library_service: SteamLibraryService::new(steam_installation),
            manifest_binding_service: ManifestBindingService::with_storage(Some(
                scratch_dir.join("pins.json"),
            )),
            hot_reload_service: HotReloadService::new()
                .set_watch_dir(PathBuf::from("~/.local/share/Steam/config/stplug-in")),
            cloud_redirect_service: CloudRedirectService::with_path(
                scratch_dir.join("cloudredirect"),
            ),
            game_injection_service: GameProcessInjectionService::with_storage(Some(
                scratch_dir.join("injections.json"),
            )),
            wired_service: WiredService::new(Some(scratch_dir.join("wired"))),
            steam_auto_crack,
            cef_bridge,
            http_server: None,
            tools_manager,
            telemetry_service,
            asset_hash_service: AssetHashService::new(),
            applied_fix_index: AppliedFixIndexService::new(None),
            tray_service,
            tray_locked: false,
            quit_requested: false,
            prev_viewport_minimized: false,
            suppress_minimize_hook_frames: 0,
            tray_hide_notified: false,
            egui_ctx: Some(ctx.clone()),
            msg_tx,
            msg_rx,
            rpc_tx,
            rpc_rx,
            active_view: ActiveView::Overview,
            command_input: String::new(),
            focus_command_next: false,
            status_text: "Ready — drop .lua, .manifest, or .zip files anywhere to install".into(),
            active_toast: None,
            millennium_connected: false,
            cover_textures: HashMap::new(),
            cover_lru: VecDeque::new(),
            cover_in_flight: HashSet::new(),
            cover_known_urls: HashMap::new(),
            cover_retry_urls: HashMap::new(),
            cover_failed_at: HashMap::new(),
            recent_installed: vec![
                (1245620, PathBuf::from("1245620.lua")),
                (1091500, PathBuf::from("1091500.lua")),
            ],
            featured_requested: true,
            fixes_requested: true,
            steam_sls_installed: true,
            cached_tool_statuses: HashMap::new(),
            cached_telemetry_events: Vec::new(),
            cached_telemetry_count: 0,
            telemetry_cache_dirty: true,
            acquire_search_query: String::new(),
            acquire_tab: AcquireTab::TopSellers,
            featured_top: Vec::new(),
            featured_new: Vec::new(),
            search_results: Vec::new(),
            acquire_selected_app: None,
            acquire_details: None,
            acquire_sources: BTreeMap::new(),
            acquire_selected_source: None,
            acquire_dlc_info: None,
            acquire_busy: false,
            acquire_progress: 0.0,
            acquire_status: "AppID 1245620: 2 provider(s) available".into(),
            library_rows: Vec::new(),
            library_filter_mode: 0,
            library_search: String::new(),
            library_selected_ids: HashSet::new(),
            depots_app_id: None,
            depots_jump_input: String::new(),
            depots_subtab: DepotsSubTab::Declarations,
            depots_lua: None,
            depots_info: None,
            depots_variants: Vec::new(),
            depots_selected_variant_hash: None,
            depots_editor_text: String::new(),
            depots_editing_live: false,
            depots_picks: Vec::new(),
            depots_dest_dir: "/home/user/LuaTools Depots".into(),
            depots_downloading: false,
            depots_dl_pct: 0.0,
            depots_dl_status: String::new(),
            binding_depot_input: String::new(),
            binding_gid_input: String::new(),
            binding_size_input: String::new(),
            fixes_listings: Vec::new(),
            fixes_tags: Vec::new(),
            fixes_selected_tag: None,
            fixes_search: String::new(),
            fixes_my_games_only: false,
            fixes_current_app: None,
            fixes_items: Vec::new(),
            fixes_selected_row: None,
            fixes_busy: false,
            fixes_progress: 0.0,
            settings_subtab: SettingsSubTab::General,
            tool_busy: HashMap::new(),
            tool_progress: HashMap::new(),
            settings_show_hubcap: false,
            settings_hubcap_status: String::new(),
            settings_usage_text: String::new(),
            settings_millennium_status:
                "Optional: Click 'Check Millennium Status' if you use Millennium Steam themes"
                    .into(),
            settings_update_status: format!("Up to date (v{}) · Checked once on startup", VERSION),
            settings_telemetry_status: String::new(),
            wired_busy: false,
            wired_progress: 0.0,
            wired_status_text: String::new(),
            bridge_system_preload: false,
            bridge_busy: false,
            bridge_status_msg: String::new(),
            cloud_app_input: String::new(),
            update_banner: UpdateBannerState::Hidden,
            guide_topic: GuideTopic::QuickStart,
            guide_textures: HashMap::new(),
            guide_zoom_screenshot: None,
            last_hot_reload_key: None,
            first_run_quick_setup: true,
            overwrite_diff_modal: OverwriteDiffModal::default(),
            download_history: Self::load_download_history_from(
                &crate::models::config::default_luatools_data_dir().join("download_history.json"),
            ),
            download_history_path: crate::models::config::default_luatools_data_dir()
                .join("download_history.json"),
            pics_drift_checked: false,
            add_appid_modal: AddByAppIdModal::default(),
            game_detail_modal: GameDetailModal::default(),
            integration_modal: IntegrationModal::default(),
            launch_options_modal: LaunchOptionsModal::default(),
            depot_detail_modal: DepotDetailModal::default(),
            text_prompt_modal: TextPromptModal::default(),
            about_modal_open: false,
            telemetry_modal_open: false,
            qa_capture: None,
        };
        app.populate_qa_demo_data();
        app.rebuild_depot_picks();
        app
    }

    fn populate_qa_demo_data(&mut self) {
        self.settings_service.settings.telemetry_consent_seen = true;
        self.telemetry_service.record(
            "info",
            "startup",
            "app_start",
            "Steam connected (2 libraries) · SLSsteam synced",
        );
        self.telemetry_service.record(
            "warn",
            "depot_dl",
            "session_hint",
            "Depot 1245621 requires cached QR session (-remember-password)",
        );
        if self.library_rows.is_empty() {
            self.library_rows = vec![
                LibraryRow {
                    app_id: 1245620,
                    name: "ELDEN RING".into(),
                    installed: true,
                    has_lua: true,
                    fake_id: Some(480),
                    has_token: true,
                    status_label: "Installed + Lua",
                    install_path: None,
                },
                LibraryRow {
                    app_id: 1091500,
                    name: "Cyberpunk 2077".into(),
                    installed: true,
                    has_lua: true,
                    fake_id: Some(480),
                    has_token: false,
                    status_label: "Installed + Lua",
                    install_path: None,
                },
                LibraryRow {
                    app_id: 1145360,
                    name: "Hades".into(),
                    installed: false,
                    has_lua: true,
                    fake_id: None,
                    has_token: false,
                    status_label: "Lua Only",
                    install_path: None,
                },
            ];
        }
        if self.featured_top.is_empty() {
            self.featured_top = vec![
                SteamFeaturedItem {
                    app_id: 1245620,
                    name: "ELDEN RING".into(),
                    large_capsule_image: String::new(),
                },
                SteamFeaturedItem {
                    app_id: 1091500,
                    name: "Cyberpunk 2077".into(),
                    large_capsule_image: String::new(),
                },
                SteamFeaturedItem {
                    app_id: 2358720,
                    name: "Black Myth: Wukong".into(),
                    large_capsule_image: String::new(),
                },
            ];
            self.acquire_selected_app = Some(1245620);
            self.acquire_details = Some(GameDetails {
                app_id: 1245620,
                name: "ELDEN RING".into(),
                app_type: "game".into(),
                base_app_id: None,
                genres: vec!["Action".into(), "RPG".into()],
                header_image: String::new(),
                release_date: "Feb 25, 2022".into(),
                is_free: false,
                // Extended fields
                windows: true,
                mac: false,
                linux: false,
                release_date_text: Some("Feb 25, 2022".into()),
                release_year: Some(2022),
                metacritic: Some(96),
                reviews: Some(100000),
                is_adult: false,
                short_description: Some("Elden Ring is an action RPG...".into()),
                developers: vec!["FromSoftware".into()],
                publishers: vec!["Bandai Namco".into()],
            });
            self.acquire_sources
                .insert("morrenus".into(), "available".into());
            self.acquire_sources
                .insert("hubcap".into(), "available".into());
            self.acquire_sources
                .insert("mirror_eu".into(), "unavailable".into());
        }
        self.depots_app_id = Some(1245620);
        let sample_lua = format!(
            "addappid(1245620) -- ELDEN RING\naddappid(1245621, 1, \"{}\")\naddappid(2778580) -- Shadow of the Erdtree\nsetManifestid(1245621, \"8492018394810293847\", 52428800000)\n",
            "a8f4c2e1".repeat(8)
        );
        let _ = self
            .lua_vault
            .save_text(1245620, &sample_lua, Some("Patch 1.16 (Pinned)"));
        self.depots_variants = self.lua_vault.get_variants(1245620);
        let _ = self.manifest_binding_service.set_pin(
            1245620,
            1245621,
            "8492018394810293847".into(),
            52_428_800_000,
        );
        self.depots_lua = Some(LuaFileParser::parse_text(&sample_lua, 1245620));
        self.depots_editor_text = sample_lua;
        self.fixes_listings = vec![DRMFixGameListing {
            app_id: "2358720".into(),
            name: "Black Myth: Wukong".into(),
            fix_count: 2,
            tags: vec![DRMFixTag {
                id: "online".into(),
                name: "Online Fix".into(),
                slug: "online".into(),
            }],
        }];
        self.fixes_tags = vec![DRMFixTag {
            id: "online".into(),
            name: "Online Fix".into(),
            slug: "online".into(),
        }];
        self.fixes_current_app = Some(2358720);
        self.fixes_items = vec![DRMFixFixItem {
            id: "fx_1".into(),
            title: "Verified Launch & Offline Bypass Patch".into(),
            description: "Backs up original binaries with SHA-256 verification before applying."
                .into(),
            tags: Vec::new(),
            has_manifest: true,
            has_fix: true,
            manifest_filename: None,
            fix_filename: None,
        }];
        self.update_banner = UpdateBannerState::Hidden;
        self.slssteam_service.config.play_not_owned = true;
        self.slssteam_service.config.additional_apps = vec![1091500, 1145360, 1245620];
        self.slssteam_service
            .set_depot_key(1245621, "a8f4c2e1".repeat(8));
        let _ = self.slssteam_service.save();
        {
            let mut st = self
                .steam_bridge_service
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            st.steam_detected = true;
            st.steam_root = Some(PathBuf::from("~/.local/share/Steam"));
            st.steam_pid = Some(48120);
            st.integration_lib_installed = true;
            st.launcher_installed = true;
            st.integration_active = true;
            st.patterns_loaded = true;
            st.steam_client_found = true;
            st.steam_ui_found = true;
            st.steam_client_sha256 =
                Some("9f4a81c0e72b3d19a4f06b82c71e5509d38a42f1c6e09b71834d2a15e8f03c91".into());
            st.steam_client_patterns = Some(
                "bis_subscribed_app, get_app_id, get_app_ownership_ticket, request_encrypted_app_ticket, get_steam_id, depot_key_integration".into(),
            );
            st.steam_ui_sha256 =
                Some("4c18b902f3e67a1180d52b49e1a37c68f2049d81a73e5c10b9248f716d3e9a04".into());
            st.steam_ui_patterns = Some("library_play_state, store_bridge_rpc".into());
            st.apps_with_tickets = vec![1245620, 1091500];
        }
        self.manifest_binding_service.set_pin(
            1245620,
            1245622,
            "3319482710493827162".into(),
            512_000_000,
        );
        self.hot_reload_service.push_synthetic_event(
            PathBuf::from("~/.local/share/Steam/config/stplug-in/1245620.lua"),
            FileChangeType::Modified,
        );
        self.cloud_redirect_service.enable_cloud_for_app(1245620);
    }

    pub fn detect_steam_and_sync(&mut self) {
        let ov = self.settings_service.settings.steam_path_override.clone();
        if let Some(inst) = self.steam_service.detect(Some(&ov)) {
            self.lua_vault.steam_installation = Some(inst.clone());
            self.depot_downloader
                .with_steam(inst.clone(), self.slssteam_service.clone());
            let installer = LuaInstaller::new(inst.clone(), self.lua_vault.clone());
            self.lua_installer = Some(installer);

            self.play_unowned_service.steam_installation = Some(inst.clone());
            self.steam_library_service.steam_installation = Some(inst.clone());
            self.steam_bridge_service
                .set_steam_installation(Some(inst.clone()));
            self.hot_reload_service
                .update_watch_dir(&inst.stplug_in_path);
            self.wired_service.steam_root = Some(inst.root.clone());
            self.api_client.set_steam_root(Some(inst.root.clone()));
            self.app_info_cache.set_steam_root(Some(inst.root.clone()));
            self.cover_cache.set_steam_root(Some(inst.root.clone()));
            if self.slssteam_service.config.play_not_owned {
                let _ = self
                    .play_unowned_service
                    .sync_plugin_file(&self.slssteam_service);
            }

            self.cef_bridge.steam_root = Some(inst.root.clone());
            if self.settings_service.settings.cef_bridge_enabled {
                self.cef_bridge.start();
            }

            let desired_port = self.settings_service.settings.http_server_port;
            if !self.settings_service.settings.http_server_enabled {
                if let Some(mut srv) = self.http_server.take() {
                    srv.stop();
                }
            } else {
                let needs_restart = self
                    .http_server
                    .as_ref()
                    .map(|s| s.port != desired_port)
                    .unwrap_or(true);
                if needs_restart {
                    if let Some(mut srv) = self.http_server.take() {
                        srv.stop();
                    }
                    let wakeup: Option<Arc<dyn Fn() + Send + Sync>> =
                        self.egui_ctx.clone().map(|c| {
                            Arc::new(move || c.request_repaint()) as Arc<dyn Fn() + Send + Sync>
                        });
                    self.http_server = HttpServerService::start(
                        desired_port,
                        inst.stplug_in_path.clone(),
                        self.rpc_tx.clone(),
                        wakeup,
                    );
                }
            }

            if self.settings_service.settings.donate_keys {
                let dk = DonateKeysService::new(inst.root.clone(), None);
                thread::spawn(move || {
                    let _ = dk.donate_new_keys();
                });
            }

            if let Some(ev) = self.hot_reload_service.recent_events().first() {
                self.last_hot_reload_key = Some(format!(
                    "{}:{}:{}",
                    ev.timestamp,
                    ev.event_type.as_str(),
                    ev.path.display()
                ));
            }
            self.refresh_all_local_state();
            if let Some(aid) = self.depots_app_id.filter(|&id| id > 0) {
                self.reload_depots_local(aid);
            }
            self.status_text = format!(
                "Steam connected at {} ({} library folder(s))",
                inst.root.display(),
                inst.libraries.len()
            );
        } else {
            self.lua_installer = None;
            self.depot_downloader.steam_installation = None;
            self.play_unowned_service.steam_installation = None;
            self.steam_library_service.steam_installation = None;
            self.steam_bridge_service.set_steam_installation(None);
            self.wired_service.steam_root = None;
            self.api_client.set_steam_root(None);
            self.app_info_cache.set_steam_root(None);
            self.cover_cache.set_steam_root(None);
            if let Some(mut srv) = self.http_server.take() {
                srv.stop();
            }
            self.recent_installed.clear();
            self.library_rows.clear();
            self.status_text =
                "Steam not detected — set your Steam folder in Settings if installed in a custom path."
                    .into();
        }
    }

    pub fn refresh_all_local_state(&mut self) {
        self.slssteam_service.reload();
        self.depot_downloader.slssteam_service = Some(self.slssteam_service.clone());
        self.applied_fix_index.rebuild_index(&self.steam_service);
        self.telemetry_service.update_runtime_context(
            self.steam_service.is_available(),
            self.steam_service.libraries().len(),
            self.slssteam_service.config.exists(),
        );
        if let Some(inst) = &self.steam_service.installation {
            if inst.root.join("config/depotcache").exists() {
                let _ = crate::services::depot_cache_migration::DepotCacheMigrationService::from_steam_service(
                    &self.steam_service,
                )
                .run();
            }
            if !self.pics_drift_checked {
                self.pics_drift_checked = true;
                let mut store = crate::services::launch_mod_store::LaunchModStore::new(None);
                let drifted_apps = crate::services::launch_mod_store::reconcile_startup_drift(
                    &self.steam_service,
                    &mut store,
                );
                if drifted_apps > 0 {
                    self.notify(
                        format!(
                            "Detected Steam PICS update on {} app(s); launch options rebased.",
                            drifted_apps
                        ),
                        ToastKind::Info,
                    );
                }
            }
        }
        let _ = self.steam_library_service.migrate_depotcache();
        if let Some(installer) = &self.lua_installer {
            self.recent_installed = installer.enumerate_installed();
        }
        self.steam_sls_installed = dirs::home_dir().is_some_and(|h| {
            h.join(".local/bin/steam-sls").is_file()
                || h.join(".local/share/SLSsteam/SLSsteam.so").is_file()
        });
        self.cached_tool_statuses.clear();
        self.telemetry_cache_dirty = true;
        self.refresh_library_rows();
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
    }

    pub fn ensure_tray_initialized(&mut self) {
        if !self.tray_service.is_tray_available() {
            let _ = self.tray_service.initialize();
        }
    }

    pub fn refresh_library_rows(&mut self) {
        let Some(inst) = &self.steam_service.installation else {
            self.library_rows.clear();
            return;
        };
        if inst.root.join("config/depotcache").exists() {
            let _ = crate::services::depot_cache_migration::DepotCacheMigrationService::from_steam_service(
                &self.steam_service,
            )
            .run();
        }

        let mut installed_names: HashMap<u32, String> = HashMap::new();
        let mut install_paths: HashMap<u32, PathBuf> = HashMap::new();
        for lib in &inst.libraries {
            for aid in crate::models::steam::scan_installed_games(lib) {
                if let Some(n) = crate::models::steam::get_game_name_from_acf(lib, aid) {
                    installed_names.insert(aid, n);
                }
                if let Some(dir) = crate::models::steam::get_install_dir_from_acf(lib, aid) {
                    install_paths.insert(aid, dir);
                }
            }
        }

        let lua_ids: HashSet<u32> = SteamService::list_lua_app_ids(&inst.stplug_in_path)
            .into_iter()
            .collect();
        let mut all_ids: Vec<u32> = installed_names
            .keys()
            .copied()
            .chain(lua_ids.iter().copied())
            .collect();
        all_ids.sort_unstable();
        all_ids.dedup();

        // Single 64 KiB streaming pass over applist.json for unresolved AppIDs only
        let unresolved: Vec<u32> = all_ids
            .iter()
            .copied()
            .filter(|aid| !installed_names.contains_key(aid))
            .collect();
        let applist_names = if unresolved.is_empty() {
            HashMap::new()
        } else {
            self.applist_cache.resolve_batch(&unresolved)
        };

        let mut rows = Vec::with_capacity(all_ids.len());
        for aid in all_ids {
            let is_inst = installed_names.contains_key(&aid);
            let has_lua = lua_ids.contains(&aid);
            let fake_id = self.slssteam_service.get_fake_app_id(aid);
            let has_token = self.slssteam_service.get_token(aid).is_some();
            let name = installed_names
                .get(&aid)
                .cloned()
                .or_else(|| applist_names.get(&aid).cloned())
                .unwrap_or_else(|| format!("App {}", aid));
            let status_label = match (is_inst, has_lua) {
                (true, true) => "Installed + Lua",
                (false, true) => "Lua Only",
                _ => "Installed",
            };
            rows.push(LibraryRow {
                app_id: aid,
                name,
                installed: is_inst,
                has_lua,
                fake_id,
                has_token,
                status_label,
                install_path: install_paths.get(&aid).cloned(),
            });
        }
        self.library_rows = rows;
        let valid_ids: HashSet<u32> = self.library_rows.iter().map(|r| r.app_id).collect();
        self.library_selected_ids
            .retain(|id| valid_ids.contains(id));
    }

    pub fn resolve_game_title(&self, app_id: u32) -> String {
        self.steam_service
            .get_game_name(app_id)
            .or_else(|| {
                self.library_rows
                    .iter()
                    .find(|r| r.app_id == app_id)
                    .map(|r| r.name.clone())
            })
            .or_else(|| {
                self.featured_top
                    .iter()
                    .chain(self.featured_new.iter())
                    .find(|i| i.app_id == app_id && !i.name.is_empty())
                    .map(|i| i.name.clone())
            })
            .or_else(|| {
                self.search_results
                    .iter()
                    .find(|i| i.app_id == app_id && !i.name.is_empty())
                    .map(|i| i.name.clone())
            })
            .or_else(|| self.applist_cache.get_name(app_id))
            .unwrap_or_else(|| format!("App {}", app_id))
    }

    /// Shows a non-blocking top-right toast notification (never placed at the bottom).
    pub fn notify(&mut self, msg: impl Into<String>, kind: ToastKind) {
        let text = msg.into();
        if kind == ToastKind::Warning {
            self.telemetry_service
                .record("warn", "diagnostic", "warning_toast", &text);
            self.telemetry_cache_dirty = true;
        }
        self.status_text = text.clone();
        self.active_toast = Some(ToastNotification {
            text,
            kind,
            created_at: std::time::Instant::now(),
            duration_secs: 4.5,
        });
    }

    // ===== Background Dispatchers =====

    pub fn probe_bridge_async(&self, ctx: &egui::Context) {
        let m = self.millennium_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let avail = m.is_available();
            let ver = avail.then(|| m.get_version()).flatten();
            let sp = avail.then(|| m.get_steam_path()).flatten();
            let _ = tx.send(AppMessage::BridgeProbed {
                available: avail,
                version: ver,
                steam_path: sp,
            });
            ctx_c.request_repaint();
        });
    }

    pub fn fetch_featured_async(&self, ctx: &egui::Context) {
        let api = self.api_client.clone();
        let hw = self.hardware_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let (mut top, mut new_r) = api.get_featured();
            top.retain(|x| !hw.is_hardware(x.app_id));
            new_r.retain(|x| !hw.is_hardware(x.app_id));
            let _ = tx.send(AppMessage::FeaturedLoaded {
                top,
                new_releases: new_r,
            });
            ctx_c.request_repaint();
        });
    }

    pub fn fetch_fix_listings_async(&self, ctx: &egui::Context) {
        let api = self.api_client.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let (games, tags) = api.get_drm_fix_listings();
            let _ = tx.send(AppMessage::FixListingsLoaded { games, tags });
            ctx_c.request_repaint();
        });
    }

    pub fn fetch_usage_async(&self, ctx: &egui::Context) {
        let api = self.api_client.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let mut parts = Vec::new();
            if let Some((u, lim)) = api.get_standard_usage() {
                parts.push(format!("Daily quota: {}/{}", u, lim));
            }
            if let Some(sup) = api.get_supporter_status() {
                parts.push(format!("Supporter: {}", if sup { "Yes" } else { "No" }));
            }
            let _ = tx.send(AppMessage::UsageLoaded(parts.join("  ·  ")));
            ctx_c.request_repaint();
        });
    }

    fn record_known_cover_url(&mut self, app_id: u32, url: &str) -> bool {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return false;
        }
        let is_low_res = {
            let lower = trimmed.to_ascii_lowercase();
            lower.contains("capsule_sm_120") || lower.contains("capsule_231x87")
        };
        if let Some(existing) = self.cover_known_urls.get(&app_id) {
            if existing == trimmed {
                return false;
            }
            let existing_low_res = {
                let lower = existing.to_ascii_lowercase();
                lower.contains("capsule_sm_120") || lower.contains("capsule_231x87")
            };
            // Don't downgrade a known high-res header/capsule URL to a small search thumbnail
            if !existing_low_res && is_low_res {
                return false;
            }
        }
        self.cover_known_urls.insert(app_id, trimmed.to_string());
        true
    }

    pub fn ensure_cover_requested(
        &mut self,
        app_id: u32,
        custom_url: Option<String>,
        ctx: &egui::Context,
    ) {
        if self.cover_textures.contains_key(&app_id) {
            return;
        }
        let clean_url = custom_url
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());

        let mut url_updated = false;
        if let Some(ref u) = clean_url {
            url_updated = self.record_known_cover_url(app_id, u);
            if url_updated {
                self.cover_failed_at.remove(&app_id);
            }
        }

        if self.cover_in_flight.contains(&app_id) {
            // Queue the newly learned URL so if the in-flight attempt fails it immediately retries
            // with the upgraded URL (but never re-queue the exact same URL already in flight).
            if url_updated {
                if let Some(u) = self.cover_known_urls.get(&app_id).cloned() {
                    self.cover_retry_urls.insert(app_id, u);
                }
            }
            return;
        }

        // Prevent per-frame retry storms when offline or for 404/unknown AppIDs (45s cooldown),
        // bypassed only when a genuinely new/higher-res URL was just learned.
        if !url_updated {
            if let Some(failed_at) = self.cover_failed_at.get(&app_id) {
                if failed_at.elapsed() < std::time::Duration::from_secs(45) {
                    return;
                }
            }
        }

        self.cover_in_flight.insert(app_id);
        let effective_url = self.cover_known_urls.get(&app_id).cloned().or(clean_url);
        let cc = self.cover_cache.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let mut loaded = false;
            if let Some(path) = cc.ensure(app_id, effective_url.as_deref()) {
                if let Ok(bytes) = fs::read(&path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        // Cards render covers at ~222x104; downscale large Steam headers/capsules
                        // to cut per-cover RGBA GPU/RAM footprint by ~6x.
                        let resized = if img.width() > 280 || img.height() > 140 {
                            img.resize(280, 140, image::imageops::FilterType::Triangle)
                        } else {
                            img
                        };
                        let rgba = resized.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let _ = tx.send(AppMessage::CoverLoaded {
                            app_id,
                            width: w as usize,
                            height: h as usize,
                            rgba: rgba.into_raw(),
                        });
                        loaded = true;
                    }
                }
            }
            if !loaded {
                let _ = tx.send(AppMessage::CoverFailed { app_id });
            }
            ctx_c.request_repaint();
        });
    }

    pub fn execute_store_search(&mut self, query: &str, ctx: &egui::Context) {
        let q = query.trim();
        if q.is_empty() {
            self.acquire_tab = AcquireTab::TopSellers;
            return;
        }
        if let Some(aid) = SteamLinkParser::app_id_from(q) {
            self.open_app_in_acquire(aid, ctx);
            return;
        }
        self.acquire_status = format!("Searching Steam Store for '{}'…", q);
        let q_owned = q.to_string();
        let api = self.api_client.clone();
        let hw = self.hardware_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            match api.search_store(&q_owned) {
                Ok(mut items) => {
                    items.retain(|x| !hw.is_hardware(x.app_id));
                    let _ = tx.send(AppMessage::SearchFinished { items, error: None });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::SearchFinished {
                        items: Vec::new(),
                        error: Some(e),
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn open_app_in_acquire(&mut self, app_id: u32, ctx: &egui::Context) {
        self.active_view = ActiveView::Acquire;
        self.acquire_selected_app = Some(app_id);
        self.acquire_details = None;
        self.acquire_sources.clear();
        self.acquire_selected_source = None;
        self.acquire_dlc_info = None;
        self.acquire_status = format!("Checking providers for AppID {}…", app_id);
        let known_url = self.cover_known_urls.get(&app_id).cloned();
        self.ensure_cover_requested(app_id, known_url, ctx);

        let info_cache = self.app_info_cache.clone();
        let api = self.api_client.clone();
        let hc = self.hubcap_service.clone();
        let hk = self.settings_service.settings.hubcap_api_key.clone();
        let signed_in = self.auth_service.is_signed_in();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        thread::spawn(move || {
            let details = info_cache.get(app_id);
            let _ = tx.send(AppMessage::AcquireDetailsLoaded {
                app_id,
                details: details.clone(),
            });
            ctx_c.request_repaint();

            let mut sources = api.check_sources(app_id);
            if HubcapService::is_valid_key_format(&hk) {
                if let Some(has_hc) = hc.check_status(&hk, app_id) {
                    sources.insert(
                        "hubcap".into(),
                        if has_hc {
                            "available".into()
                        } else {
                            "unavailable".into()
                        },
                    );
                }
            }
            let dlc_info = details
                .as_ref()
                .filter(|d| d.is_dlc() && signed_in)
                .and_then(|d| d.base_app_id)
                .and_then(|base| api.get_dlc_info(app_id, base));

            let _ = tx.send(AppMessage::AcquireSourcesLoaded {
                app_id,
                sources,
                dlc_info,
            });
            ctx_c.request_repaint();
        });
    }

    pub fn start_acquire_download(&mut self, app_id: u32, source: String, ctx: &egui::Context) {
        if self.lua_installer.is_none() {
            let msg = "Steam installation not detected.";
            self.acquire_status = msg.into();
            self.notify(msg, ToastKind::Warning);
            return;
        }
        if source != "hubcap" && !self.auth_service.is_signed_in() {
            let msg =
                "Sign in with Discord in Settings › Account first (or configure a Hubcap key).";
            self.acquire_status = msg.into();
            self.notify(msg, ToastKind::Warning);
            return;
        }
        self.acquire_busy = true;
        self.acquire_progress = 0.0;
        self.acquire_status = format!("Downloading manifest package from {}…", source);

        let api = self.api_client.clone();
        let hc = self.hubcap_service.clone();
        let hk = self.settings_service.settings.hubcap_api_key.clone();
        let gname = self.acquire_details.as_ref().map(|d| d.name.clone());
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        thread::spawn(move || {
            let tmp = std::env::temp_dir().join(format!("luatools_dl_{}", app_id));
            let _ = fs::create_dir_all(&tmp);
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            let res = if source == "hubcap" {
                hc.download_manifest(app_id, &hk, &tmp, move |p, m| {
                    let _ = tx_p.send(AppMessage::AcquireProgress { pct: p, msg: m });
                    ctx_p.request_repaint();
                })
            } else {
                api.download_manifest(app_id, &source, gname.as_deref(), &tmp, move |p, m| {
                    let _ = tx_p.send(AppMessage::AcquireProgress { pct: p, msg: m });
                    ctx_p.request_repaint();
                })
            };

            match res {
                Ok(path) => {
                    let _ = tx.send(AppMessage::AcquireInstallFinished {
                        app_id,
                        ok: true,
                        downloaded_path: Some(path),
                        source,
                        msg: "Downloaded".into(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::AcquireInstallFinished {
                        app_id,
                        ok: false,
                        downloaded_path: None,
                        source,
                        msg: e,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn start_dlc_generation(&mut self, app_id: u32, ctx: &egui::Context) {
        let Some(base_id) = self.acquire_details.as_ref().and_then(|d| d.base_app_id) else {
            return;
        };
        if !self.auth_service.is_signed_in() {
            let msg = "Sign in with Discord in Settings › Account first.";
            self.acquire_status = msg.into();
            self.notify(msg, ToastKind::Warning);
            return;
        }
        self.acquire_busy = true;
        self.acquire_progress = 0.0;
        self.acquire_status = format!("Generating DLC manifest for AppID {}…", app_id);
        let api = self.api_client.clone();
        let gname = self.acquire_details.as_ref().map(|d| d.name.clone());
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        thread::spawn(move || {
            let tmp = std::env::temp_dir().join(format!("luatools_dlc_{}", app_id));
            let _ = fs::create_dir_all(&tmp);
            match api.generate_dlc(app_id, base_id, gname.as_deref(), &tmp, |_, _| {}) {
                Ok(p) => {
                    let _ = tx.send(AppMessage::AcquireInstallFinished {
                        app_id,
                        ok: true,
                        downloaded_path: Some(p),
                        source: "dlc-gen".into(),
                        msg: "Generated DLC lua".into(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::AcquireInstallFinished {
                        app_id,
                        ok: false,
                        downloaded_path: None,
                        source: "dlc-gen".into(),
                        msg: e,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn open_game_in_depots(&mut self, app_id: u32, ctx: &egui::Context) {
        let changed_app = self.depots_app_id != Some(app_id);
        self.active_view = ActiveView::Depots;
        self.depots_app_id = Some(app_id);
        self.depots_editing_live = false;
        self.depots_selected_variant_hash = None;
        self.depots_info = None;
        if changed_app && !self.depots_downloading {
            self.depots_dl_status.clear();
        }

        self.lua_vault.adopt_loose_build_luas(app_id);
        self.lua_vault.sync_default_from_live(app_id);
        self.reload_depots_local(app_id);

        let di = self.depot_info_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let info = di.get(app_id);
            let _ = tx.send(AppMessage::DepotInfoLoaded { app_id, info });
            ctx_c.request_repaint();
        });
    }

    pub fn refresh_depot_info_async(&mut self, app_id: u32, ctx: &egui::Context) {
        self.depot_info_service.invalidate(app_id);
        let di = self.depot_info_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let info = di.get(app_id);
            let _ = tx.send(AppMessage::DepotInfoLoaded { app_id, info });
            ctx_c.request_repaint();
        });
        self.notify(
            format!("Refreshing depot metadata for AppID {}…", app_id),
            ToastKind::Info,
        );
    }

    pub fn reload_depots_local(&mut self, app_id: u32) {
        let live_text = self.lua_vault.read_live_text(app_id);
        self.depots_lua =
            (!live_text.trim().is_empty()).then(|| LuaFileParser::parse_text(&live_text, app_id));
        if let Some(ref lua) = self.depots_lua {
            self.manifest_binding_service.sync_from_lua(app_id, lua);
        }
        if !self.depots_editing_live {
            self.depots_editor_text = live_text;
        }
        self.depots_variants = self.lua_vault.get_variants(app_id);
        if self.depots_subtab == DepotsSubTab::DownloadPicker {
            self.rebuild_depot_picks();
        }
    }

    pub fn rebuild_depot_picks(&mut self) {
        self.depots_picks.clear();
        let (Some(aid), Some(lua)) = (self.depots_app_id, self.depots_lua.clone()) else {
            return;
        };
        let depot_map: HashMap<u32, ContentDepot> = self
            .depots_info
            .as_ref()
            .map(|i| i.depots.iter().map(|d| (d.id, d.clone())).collect())
            .unwrap_or_default();
        let keys = DepotDownloaderService::resolve_keys(
            self.steam_service.installation.as_ref(),
            Some(&self.slssteam_service),
            Some(&self.steam_bridge_service),
            aid,
        );

        let disabled_depot_ids: std::collections::HashSet<u32> =
            lua.disabled_entries.iter().map(|de| de.id).collect();

        for entry in lua.entries {
            if disabled_depot_ids.contains(&entry.id) {
                continue;
            }
            if entry.id == aid && entry.manifest_id.is_none() && !entry.has_key() {
                continue;
            }
            // Skip keyless/manifestless DLC AppID entitlements that are not content depots
            if entry.id != aid
                && !entry.has_key()
                && entry.manifest_id.is_none()
                && entry.commented_manifest_id.is_none()
                && !depot_map.contains_key(&entry.id)
            {
                continue;
            }
            let depot = depot_map.get(&entry.id).cloned().unwrap_or(ContentDepot {
                id: entry.id,
                size: entry.size_on_disk.unwrap_or(0),
                dlc_app_id: None,
                is_shared: false,
                os: None,
                arch: None,
                language: None,
                public_manifest_id: None,
                from_app_id: None,
            });
            let manifest_id = entry
                .manifest_id
                .clone()
                .or_else(|| entry.commented_manifest_id.clone())
                .or_else(|| depot.public_manifest_id.clone());
            let has_key = entry.key.is_some() || keys.contains_key(&entry.id);

            let blocked_reason = if depot.is_shared {
                Some("Shared redistributable".into())
            } else {
                None
            };

            let manifest_path = manifest_id.as_deref().and_then(|gid| {
                DepotDownloaderService::resolve_manifest_path(
                    self.steam_service.installation.as_ref(),
                    entry.id,
                    gid,
                )
            });

            let is_selected = blocked_reason.is_none() && !depot.is_shared && !depot.is_dlc();
            let size = entry.size_on_disk.unwrap_or(depot.size);
            self.depots_picks.push(DepotPickRow {
                depot,
                lua_entry: Some(entry),
                manifest_id,
                manifest_path,
                has_key,
                size,
                blocked_reason,
                is_selected,
            });
        }
    }

    pub fn open_app_in_fixes(&mut self, app_id: u32, ctx: &egui::Context) {
        self.active_view = ActiveView::Fixes;
        self.fixes_current_app = Some(app_id);
        self.fixes_items.clear();
        self.fixes_selected_row = None;

        let api = self.api_client.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let fixes = api.get_drm_fix_fixes(app_id);
            let _ = tx.send(AppMessage::FixesLoaded { app_id, fixes });
            ctx_c.request_repaint();
        });
    }

    pub fn apply_fix_manifest_async(
        &mut self,
        app_id: u32,
        fix: DRMFixFixItem,
        ctx: &egui::Context,
    ) {
        if !self.auth_service.is_signed_in() {
            self.notify(
                "Sign in with Discord in Settings › Account first.",
                ToastKind::Warning,
            );
            return;
        }
        self.fixes_busy = true;
        self.fixes_progress = 0.0;
        let api = self.api_client.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let tmp = std::env::temp_dir().join(format!("lt_fx_mf_{}", app_id));
            let _ = fs::create_dir_all(&tmp);
            let fb = fix
                .manifest_filename
                .clone()
                .unwrap_or_else(|| format!("{}.zip", app_id));
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            match api.download_drm_fix_slot(&fix.id, "manifest", &tmp, &fb, move |p, m| {
                let _ = tx_p.send(AppMessage::FixProgress { pct: p, msg: m });
                ctx_p.request_repaint();
            }) {
                Ok(dl) => {
                    let _ = tx.send(AppMessage::FixActionFinished {
                        app_id,
                        ok: true,
                        downloaded_manifest: Some(dl),
                        fix_id: fix.id,
                        msg: format!("Downloaded manifest for {}", fix.title),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::FixActionFinished {
                        app_id,
                        ok: false,
                        downloaded_manifest: None,
                        fix_id: fix.id,
                        msg: e,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn apply_fix_files_async(
        &mut self,
        app_id: u32,
        fix: DRMFixFixItem,
        install_dir: PathBuf,
        ctx: &egui::Context,
    ) {
        if !self.auth_service.is_signed_in() {
            self.notify(
                "Sign in with Discord in Settings › Account first.",
                ToastKind::Warning,
            );
            return;
        }
        self.fixes_busy = true;
        self.fixes_progress = 0.0;
        let api = self.api_client.clone();
        let ahs = self.asset_hash_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            let res =
                FixesService::apply_drm_fix_fix(&api, app_id, &fix, &install_dir, move |p, m| {
                    let _ = tx_p.send(AppMessage::FixProgress { pct: p, msg: m });
                    ctx_p.request_repaint();
                });
            if res.ok {
                for rel in &res.applied_files {
                    let p = install_dir.join(rel);
                    if p.is_file() {
                        let _ = ahs.hash_file(app_id, &p, rel);
                    }
                }
            }
            let _ = tx.send(AppMessage::FixActionFinished {
                app_id,
                ok: res.ok,
                downloaded_manifest: None,
                fix_id: fix.id,
                msg: if res.ok {
                    format!("Applied {} file(s)", res.applied_files.len())
                } else {
                    res.error.unwrap_or_else(|| "Failed".into())
                },
            });
            ctx_c.request_repaint();
        });
    }

    pub fn update_external_tool_async(&mut self, key: &str, ctx: &egui::Context) {
        self.tool_busy.insert(key.to_string(), true);
        self.tool_progress.insert(key.to_string(), 0.0);
        let mgr = self.tools_manager.clone();
        let k_owned = key.to_string();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        thread::spawn(move || {
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            let k_p = k_owned.clone();
            match mgr.update_tool(&k_owned, move |pct, msg| {
                let _ = tx_p.send(AppMessage::ToolProgress {
                    tool_key: k_p.clone(),
                    pct,
                    msg,
                });
                ctx_p.request_repaint();
            }) {
                Ok(exe) => {
                    let mut msg = format!("Ready at {}", exe.display());
                    if k_owned == "goldberg"
                        && crate::services::wired_service::find_7z_bin().is_none()
                    {
                        msg = format!(
                            "Goldberg Linux binaries ready at {}, but 7z/p7zip was missing on host (Windows DLLs for Proton not extracted).",
                            exe.display()
                        );
                    }
                    let _ = tx.send(AppMessage::ToolFinished {
                        tool_key: k_owned,
                        ok: true,
                        msg,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::ToolFinished {
                        tool_key: k_owned,
                        ok: false,
                        msg: e,
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn run_steamless_on_game_async(
        &mut self,
        app_id: u32,
        install_dir: PathBuf,
        ctx: &egui::Context,
    ) {
        self.tool_busy.insert("stub_remover".to_string(), true);
        self.status_text = format!(
            "Running Steamless (native) on AppID {} ({})…",
            app_id,
            install_dir.display()
        );
        let steamless_service = SteamlessService::new(self.depot_downloader.clone());
        let ahs = self.asset_hash_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        thread::spawn(move || {
            // Try native steamflatline first
            let res = steamless_service.patch_game_native(app_id, &install_dir);
            let mut total_unpacked = res.patched as usize;
            let mut native_success = res.error.is_none() && res.patched > 0;

            // If native didn't patch anything, fall back to CLI tool
            if !native_success {
                let cli_res = steamless_service.patch_game(app_id, &install_dir);
                total_unpacked += cli_res.patched as usize;
                native_success = cli_res.error.is_none() && cli_res.patched > 0;
            }

            // Also scan for any .unpacked.exe files that might have been created
            let mut unpacked_count = 0usize;
            if let Ok(entries) = walkdir::WalkDir::new(&install_dir)
                .max_depth(4)
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
            {
                for entry in entries {
                    let p = entry.path();
                    if p.is_file() {
                        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                            if name.ends_with(".unpacked.exe") {
                                let orig_path = if p.to_string_lossy().ends_with(".exe.unpacked.exe") {
                                    PathBuf::from(p.to_string_lossy().trim_end_matches(".unpacked.exe"))
                                } else {
                                    p.with_extension("exe")
                                };
                                let bak_path = PathBuf::from(format!("{}.orig.bak", orig_path.display()));
                                if !bak_path.exists() && orig_path.is_file() {
                                    let _ = fs::copy(&orig_path, &bak_path);
                                }
                                if fs::rename(p, &orig_path).is_ok() {
                                    unpacked_count += 1;
                                    let _ = ahs.hash_file(
                                        app_id,
                                        &orig_path,
                                        orig_path.file_name().and_then(|s| s.to_str()).unwrap_or(name),
                                    );
                                }
                            }
                        }
                    }
                }
            }

            total_unpacked += unpacked_count;
            let (ok, msg) = if total_unpacked > 0 {
                (
                    true,
                    format!(
                        "Steamless unpacked {}/{} executable(s) for AppID {}.",
                        total_unpacked, res.total, app_id
                    ),
                )
            } else {
                (
                    false,
                    format!(
                        "No SteamStub protection unpacked across {} executable(s) for AppID {}.",
                        res.total, app_id
                    ),
                )
            };
            let _ = tx.send(AppMessage::ToolFinished {
                tool_key: "stub_remover".to_string(),
                ok,
                msg,
            });
            ctx_c.request_repaint();
        });
    }

    pub fn verify_game_integrity_async(
        &mut self,
        app_id: u32,
        install_dir: PathBuf,
        ctx: &egui::Context,
    ) {
        let ahs = self.asset_hash_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let existing = ahs.get_hashes(app_id);
            let mut recorded_paths: HashSet<String> = HashSet::with_capacity(existing.len());
            let mut verified = 0usize;
            let mut baselined = 0usize;
            let mut failed = 0usize;

            for h in &existing {
                recorded_paths.insert(h.file_path.clone());
                let p = PathBuf::from(&h.file_path);
                if ahs.verify_file(app_id, &p) {
                    verified += 1;
                } else {
                    failed += 1;
                }
            }

            let mut scanned_binaries = 0usize;
            for entry in walkdir::WalkDir::new(&install_dir)
                .max_depth(3)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_file() {
                    let p = entry.path();
                    let name = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if name.ends_with(".exe")
                        || name.ends_with(".dll")
                        || name.ends_with(".so")
                    {
                        scanned_binaries += 1;
                        let key_path = p.to_string_lossy().to_string();
                        if !recorded_paths.contains(&key_path) {
                            let h = ahs.hash_file(app_id, p, &name);
                            if !h.sha256.is_empty() {
                                baselined += 1;
                            } else {
                                failed += 1;
                            }
                            recorded_paths.insert(key_path);
                        }
                        if scanned_binaries >= 40 {
                            break;
                        }
                    }
                }
            }

            let total = verified + baselined + failed;
            let ok = failed == 0 && total > 0;
            let msg = if total == 0 {
                format!("No binaries found under {} to hash.", install_dir.display())
            } else if verified == 0 && failed == 0 {
                format!(
                    "Baseline recorded for {} file(s) (AppID {}).",
                    baselined, app_id
                )
            } else if baselined == 0 {
                format!(
                    "Verified {} file(s) ({} changed/missing) (AppID {}).",
                    verified, failed, app_id
                )
            } else {
                format!(
                    "Verified {} file(s) ({} changed/missing), baseline recorded for {} new file(s) (AppID {}).",
                    verified, failed, baselined, app_id
                )
            };
            let _ = tx.send(AppMessage::GameIntegrityChecked {
                app_id,
                checked: total,
                ok,
                msg,
            });
            ctx_c.request_repaint();
        });
    }

    pub fn trigger_donate_keys(&mut self, ctx: &egui::Context) {
        let Some(inst) = &self.steam_service.installation else {
            self.notify("Steam installation not detected.", ToastKind::Warning);
            return;
        };
        let dk = DonateKeysService::new(inst.root.clone(), None);
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let n = dk.donate_new_keys();
            let _ = tx.send(AppMessage::DonatedKeys(n));
            ctx_c.request_repaint();
        });
    }

    pub fn check_app_update_once_async(&mut self, ctx: &egui::Context) {
        let repo_ov = self.settings_service.settings.update_repo_dir.clone();
        let rem_ov = self.settings_service.settings.git_remote_url.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        self.settings_update_status = "Checking Git remote for updates…".into();
        thread::spawn(move || {
            let res = UpdateService::check_once(&repo_ov, &rem_ov);
            let _ = tx.send(AppMessage::UpdateCheckFinished(res));
            ctx_c.request_repaint();
        });
    }

    pub fn trigger_git_self_update(&mut self, ctx: &egui::Context) {
        if matches!(self.update_banner, UpdateBannerState::Updating { .. }) {
            return;
        }
        let repo_ov = self.settings_service.settings.update_repo_dir.clone();
        let rem_ov = self.settings_service.settings.git_remote_url.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        self.update_banner = UpdateBannerState::Updating {
            pct: 0.05,
            step: "Starting Git pull & update…".into(),
        };
        self.status_text = "Updating from Git…".into();
        self.settings_update_status = "Pulling from Git & rebuilding release binary…".into();

        thread::spawn(move || {
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            let outcome = UpdateService::pull_and_update(&repo_ov, &rem_ov, move |pct, msg| {
                let _ = tx_p.send(AppMessage::UpdateProgress { pct, msg });
                ctx_p.request_repaint();
            });
            let _ = tx.send(AppMessage::UpdateApplyFinished(outcome));
            ctx_c.request_repaint();
        });
    }

    pub fn trigger_restart_steam(&mut self, ctx: &egui::Context) {
        let m = self.millennium_service.clone();
        let steam_root = self.steam_bridge_service.status().steam_root;
        let launcher = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".local/bin/luatools-steam-launcher");
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        self.status_text = "Restarting Steam…".into();
        thread::spawn(move || {
            // Try Millennium RPC first when running; otherwise gracefully shut down & relaunch Steam directly
            if m.restart_steam().is_ok() {
                let _ = tx.send(AppMessage::SteamRestarted {
                    ok: true,
                    msg: "Restarting Steam via Millennium RPC…".into(),
                });
                ctx_c.request_repaint();
                return;
            }

            let steam_cmd = steam_root
                .as_ref()
                .map(|r| r.join("steam.sh"))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| std::path::PathBuf::from("steam"));

            if crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
                let _ = std::process::Command::new(&steam_cmd)
                    .arg("-shutdown")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                for _ in 0..40 {
                    if crate::steam_bridge::preload_loader::find_steam_pid().is_none() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                if crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
                    let _ = crate::steam_bridge::preload_loader::kill_all_steam_instances();
                }
            }

            let steam_sls = dirs::home_dir()
                .map(|h| h.join(".local/bin/steam-sls"))
                .filter(|p| p.is_file());
            let launch_bin = if let Some(sls) = steam_sls {
                sls
            } else if launcher.is_file() {
                launcher
            } else {
                steam_cmd
            };
            match std::process::Command::new(&launch_bin)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: true,
                        msg: format!("Restarted Steam via {}.", launch_bin.display()),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: false,
                        msg: format!("Failed to restart Steam: {}", e),
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    /// Force-terminates all running Steam client, webhelper, overlay, and Steam Runtime instances.
    pub fn trigger_kill_steam(&mut self, ctx: &egui::Context) {
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        self.status_text = "Stopping all Steam instances…".into();
        thread::spawn(move || {
            match crate::steam_bridge::preload_loader::kill_all_steam_instances() {
                Ok(0) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: true,
                        msg: "No running Steam instances found.".into(),
                    });
                }
                Ok(count) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: true,
                        msg: format!(
                            "Closed all running Steam instances ({} process(es)).",
                            count
                        ),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::SteamRestarted { ok: false, msg: e });
                }
            }
            ctx_c.request_repaint();
        });
    }

    /// Launch (or restart) Steam using the bridge launcher (~/.local/bin/luatools-steam-launcher)
    /// which preloads libsteam_integration.so into the Steam client process. No root required.
    pub fn launch_steam_with_bridge(&mut self, ctx: &egui::Context) {
        let bridge_st = self.steam_bridge_service.status();
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let launcher = home.join(".local/bin/luatools-steam-launcher");
        let steam_sls_exists = home.join(".local/bin/steam-sls").is_file();

        if !steam_sls_exists && !launcher.exists() && !bridge_st.system_preload_enabled {
            self.notify(
                "Steam unlocker not set up yet. Install SLSsteam in Settings › Steam Setup first.",
                ToastKind::Warning,
            );
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Integrations;
            return;
        }

        let live_running = crate::steam_bridge::preload_loader::find_steam_pid().is_some();
        self.status_text = if live_running {
            "Restarting Steam with integration bridge…".into()
        } else {
            "Launching Steam with integration bridge…".into()
        };

        let steam_root = bridge_st.steam_root;
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let steam_cmd = steam_root
                .as_ref()
                .map(|r| r.join("steam.sh"))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| std::path::PathBuf::from("steam"));

            if crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
                let _ = std::process::Command::new(&steam_cmd)
                    .arg("-shutdown")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                for _ in 0..40 {
                    if crate::steam_bridge::preload_loader::find_steam_pid().is_none() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                if crate::steam_bridge::preload_loader::find_steam_pid().is_some() {
                    let _ = crate::steam_bridge::preload_loader::kill_all_steam_instances();
                }
            }
            let steam_sls = dirs::home_dir()
                .map(|h| h.join(".local/bin/steam-sls"))
                .filter(|p| p.is_file());
            let used_steam_sls = steam_sls.is_some();
            let launch_bin = if let Some(sls) = steam_sls {
                sls
            } else if launcher.is_file() {
                launcher
            } else {
                steam_cmd
            };
            match std::process::Command::new(&launch_bin)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    let msg = if used_steam_sls {
                        "Steam started via steam-sls (SLSsteam LD_AUDIT active).".into()
                    } else {
                        "Steam started with libsteam_integration.so loaded.".into()
                    };
                    let _ = tx.send(AppMessage::SteamRestarted { ok: true, msg });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: false,
                        msg: format!("Failed to launch Steam with bridge: {}", e),
                    });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn trigger_wired_action(&mut self, action: &'static str, ctx: &egui::Context) {
        if self.wired_busy {
            return;
        }
        self.wired_busy = true;
        self.wired_progress = 0.02;
        self.wired_status_text = format!("Starting Wired ({})…", action);

        let wired = self.wired_service.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let tx_p = tx.clone();
            let ctx_p = ctx_c.clone();
            let res: Result<String, String> = match action {
                "accela" => wired
                    .install_accela(|pct, msg| {
                        let _ = tx_p.send(AppMessage::WiredProgress { pct, msg });
                        ctx_p.request_repaint();
                    })
                    .map(|p| format!("ACCELA installed at {}", p.display())),
                "slssteam" => wired
                    .install_slssteam(|pct, msg| {
                        let _ = tx_p.send(AppMessage::WiredProgress { pct, msg });
                        ctx_p.request_repaint();
                    })
                    .map(|p| format!("SLSsteam installed ({})", p.display())),
                _ => wired
                    .install_all(|pct, msg| {
                        let _ = tx_p.send(AppMessage::WiredProgress { pct, msg });
                        ctx_p.request_repaint();
                    })
                    .map(|()| "Installed ACCELA + SLSsteam (Wired Suite ready)".into()),
            };
            let _ = tx.send(AppMessage::WiredFinished {
                ok: res.is_ok(),
                msg: res.unwrap_or_else(|e| e),
            });
            ctx_c.request_repaint();
        });
    }

    pub fn trigger_bridge_action_async(&mut self, action: &'static str, ctx: &egui::Context) {
        if self.bridge_busy {
            return;
        }
        self.bridge_busy = true;
        self.bridge_status_msg = match action {
            "scan" => "Scanning Steam ELF64 binaries & querying pattern tracker…".into(),
            "uninstall" => "Removing Steam LD_PRELOAD integration…".into(),
            _ => "Staging libsteam_integration.so & generating launcher…".into(),
        };

        let mut bridge = self.steam_bridge_service.clone();
        let sys_preload = self.bridge_system_preload;
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            let outcome: Result<String, String> = match action {
                "scan" => bridge.scan_and_load_patterns().map(|r| r.message),
                "uninstall" => bridge
                    .uninstall_integration()
                    .map(|()| "Uninstalled Steam integration library and launcher.".into()),
                _ => bridge.install_integration(sys_preload).and_then(|r| {
                    if r.success {
                        Ok(r.message)
                    } else {
                        Err(r.message)
                    }
                }),
            };
            match outcome {
                Ok(msg) => {
                    let _ = tx.send(AppMessage::BridgeActionFinished { ok: true, msg });
                }
                Err(msg) => {
                    let _ = tx.send(AppMessage::BridgeActionFinished { ok: false, msg });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn load_download_history_from(path: &Path) -> VecDeque<DownloadHistoryEntry> {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(mut list) = serde_json::from_str::<VecDeque<DownloadHistoryEntry>>(&raw) {
                while list.len() > 100 {
                    list.pop_back();
                }
                return list;
            }
        }
        VecDeque::new()
    }

    pub fn record_download_history(
        &mut self,
        app_id: u32,
        label: impl Into<String>,
        source: impl Into<String>,
        status: impl Into<String>,
    ) {
        let entry = DownloadHistoryEntry {
            app_id,
            label: label.into(),
            source: source.into(),
            status: status.into(),
            timestamp: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        };
        self.download_history.push_front(entry);
        while self.download_history.len() > 100 {
            self.download_history.pop_back();
        }
        if let Some(parent) = self.download_history_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.download_history) {
            let _ = fs::write(&self.download_history_path, json);
        }
    }

    pub fn check_lua_overwrite_diff(
        &self,
        path: &Path,
        expected_app_id: Option<u32>,
    ) -> Option<(u32, String, String, Vec<String>, Vec<String>)> {
        let installer = self.lua_installer.as_ref()?;
        let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let lower = fname.to_ascii_lowercase();
        let mut new_lua_text: Option<String> = None;
        let mut aid = expected_app_id;

        if lower.ends_with(".lua") {
            let txt = fs::read_to_string(path).ok()?;
            if aid.is_none() {
                aid = fname
                    .strip_suffix(".lua")
                    .and_then(|s| s.split('_').next())
                    .and_then(|s| s.parse::<u32>().ok())
                    .or_else(|| LuaFileParser::infer_app_id_from_text(&txt));
            }
            new_lua_text = Some(txt);
        } else if lower.ends_with(".zip") {
            if let Ok(f) = fs::File::open(path) {
                if let Ok(mut zip) = zip::ZipArchive::new(f) {
                    for i in 0..zip.len() {
                        if let Ok(mut entry) = zip.by_index(i) {
                            let ename = entry.name().to_string();
                            if ename.to_ascii_lowercase().ends_with(".lua") {
                                let mut buf = String::new();
                                if std::io::Read::read_to_string(&mut entry, &mut buf).is_ok() {
                                    if aid.is_none() {
                                        let stem = Path::new(&ename)
                                            .file_stem()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("");
                                        aid = stem
                                            .split('_')
                                            .next()
                                            .and_then(|s| s.parse::<u32>().ok())
                                            .or_else(|| LuaFileParser::infer_app_id_from_text(&buf));
                                    }
                                    new_lua_text = Some(buf);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            if new_lua_text.is_none() {
                if let Ok(txt) = fs::read_to_string(path) {
                    aid = aid.or_else(|| LuaFileParser::infer_app_id_from_text(&txt));
                    new_lua_text = Some(txt);
                }
            }
        }

        let target_aid = aid?;
        let new_txt = new_lua_text?;
        let old_txt = installer.read_lua_text(target_aid)?;
        if old_txt.trim() == new_txt.trim() {
            return None;
        }

        let old_set: HashSet<&str> = old_txt
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let new_set: HashSet<&str> = new_txt
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();

        let added: Vec<String> = new_txt
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !old_set.contains(l))
            .take(40)
            .map(|l| format!("+ {}", l))
            .collect();
        let removed: Vec<String> = old_txt
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !new_set.contains(l))
            .take(40)
            .map(|l| format!("- {}", l))
            .collect();

        if added.is_empty() && removed.is_empty() {
            return None;
        }
        Some((target_aid, old_txt, new_txt, added, removed))
    }

    pub fn handle_protocol_url(&mut self, url: &str, ctx: &egui::Context) {
        if let Some(bst) = crate::steam_bridge::token_bridge::BstUri::parse(url) {
            if bst.is_valid() {
                self.slssteam_service
                    .set_token(bst.app_id, bst.token.clone());
                let _ = self.slssteam_service.save();
                self.notify(
                    format!(
                        "Saved BST token for AppID {} into SLSsteam config.yaml.",
                        bst.app_id
                    ),
                    ToastKind::Success,
                );
            }
        }

        let Some(act) = ProtocolService::parse(url) else {
            self.notify(
                format!("Unrecognized luatools:// URL: {}", url),
                ToastKind::Warning,
            );
            return;
        };

        if act.silent {
            if self.can_hide_to_tray() {
                self.tray_service.set_visibility(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.tray_service.set_visibility(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }

        if let Some(ref code) = act.redeem_code {
            let code_str = code.trim().to_string();
            // 1. Direct APPID:TOKEN or APPID/TOKEN syntax (e.g. "1245620:abc123token")
            let mut resolved_direct = None;
            if let Some((aid_s, tok)) = code_str.split_once(':').or_else(|| code_str.split_once('/')) {
                if let Ok(aid) = aid_s.trim().parse::<u32>() {
                    let t = tok.trim();
                    if aid > 0 && !t.is_empty() {
                        resolved_direct = Some((aid, t.to_string(), None::<String>, None::<String>));
                    }
                }
            }

            // 2. If not direct, query the online redemption endpoint
            let redemption_result = if let Some(direct) = resolved_direct {
                Some(direct)
            } else {
                let agent = ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(6))
                    .build();
                let endpoints = [
                    "https://luastools.xyz/drm/redeem",
                    "https://tokeer.luastools.xyz/redeem",
                ];
                let mut res = None;
                for ep in &endpoints {
                    let resp = agent
                        .post(ep)
                        .set("Content-Type", "application/json")
                        .send_json(serde_json::json!({ "code": code_str }));
                    if let Ok(r) = resp {
                        if let Ok(val) = r.into_json::<serde_json::Value>() {
                            let app_id = val.get("app_id")
                                .or_else(|| val.get("appid"))
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32)
                                .unwrap_or(0);
                            let token = val.get("token")
                                .or_else(|| val.get("app_token"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| code_str.clone());
                            let app_ticket = val.get("appticket")
                                .or_else(|| val.get("app_ticket"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let e_ticket = val.get("eticket")
                                .or_else(|| val.get("e_ticket"))
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            if app_id > 0 {
                                res = Some((app_id, token, app_ticket, e_ticket));
                                break;
                            }
                        }
                    }
                }
                res
            };

            if let Some((aid, tok, app_ticket, e_ticket)) = redemption_result {
                self.slssteam_service.set_token(aid, tok);
                let _ = self.slssteam_service.save();

                if app_ticket.is_some() || e_ticket.is_some() {
                    let ticket_data = crate::steam_bridge::ticket_store::AppTicketData {
                        app_id: aid,
                        steam_id: None,
                        app_ticket,
                        e_ticket,
                        last_update: Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                        ),
                    };
                    let _ = self.steam_bridge_service.set_tickets(ticket_data);
                }

                self.notify(
                    format!(
                        "Redeemed code '{}' for AppID {}. Persisted into SLSsteam config.yaml and ticket store.",
                        code_str, aid
                    ),
                    ToastKind::Success,
                );
            } else {
                self.notify(
                    format!("Received redeem code '{}' via protocol handler.", code_str),
                    ToastKind::Info,
                );
            }
            return;
        }

        if let Some(ref tok) = act.token {
            if act.app_id > 0 && !tok.is_empty() {
                self.slssteam_service.set_token(act.app_id, tok.clone());
                let _ = self.slssteam_service.save();
                self.notify(
                    format!(
                        "Persisted AppToken for AppID {} into SLSsteam config.yaml.",
                        act.app_id
                    ),
                    ToastKind::Success,
                );
            }
        }

        match act.kind.as_str() {
            "game" => {
                self.open_app_in_acquire(act.app_id, ctx);
                if let Some(src) = act.source {
                    self.acquire_selected_source = Some(src);
                }
            }
            "install" => {
                self.open_app_in_acquire(act.app_id, ctx);
                let src = if let Some(s) = act.source {
                    self.acquire_selected_source = Some(s.clone());
                    s
                } else if HubcapService::is_valid_key_format(
                    &self.settings_service.settings.hubcap_api_key,
                ) {
                    "hubcap".to_string()
                } else {
                    "morrenus".to_string()
                };
                self.start_acquire_download(act.app_id, src, ctx);
            }
            "manage" => self.open_game_in_depots(act.app_id, ctx),
            "fix" => self.open_app_in_fixes(act.app_id, ctx),
            _ => {
                self.notify(
                    format!("Unsupported luatools:// action: {}", act.kind),
                    ToastKind::Warning,
                );
            }
        }
    }

    pub fn install_dropped_paths(&mut self, paths: &[PathBuf]) {
        let Some(installer) = self.lua_installer.clone() else {
            self.notify("Steam installation not detected yet.", ToastKind::Warning);
            return;
        };
        let mut ok_count = 0usize;
        let mut last_err = None;
        for p in paths {
            if let Some((aid, old_txt, new_txt, added, removed)) =
                self.check_lua_overwrite_diff(p, None)
            {
                self.overwrite_diff_modal = OverwriteDiffModal {
                    open: true,
                    app_id: aid,
                    source_label: "drop".into(),
                    old_content: old_txt,
                    new_content: new_txt,
                    added_lines: added,
                    removed_lines: removed,
                    pending_package_path: Some(p.clone()),
                    cleanup_parent: false,
                };
                continue;
            }
            let res = installer.install_any(
                p,
                None,
                false,
                Some("drop"),
                Some(&mut self.settings_service),
                Some(&mut self.slssteam_service),
            );
            if res.ok {
                ok_count += 1;
            } else {
                last_err = res.error;
            }
        }
        self.refresh_all_local_state();
        if ok_count > 0 {
            self.notify(
                format!("Installed {} package(s) into Steam.", ok_count),
                ToastKind::Success,
            );
        } else if let Some(e) = last_err {
            self.notify(format!("Install error: {}", e), ToastKind::Warning);
        }
    }

    fn pump_messages(&mut self, ctx: &egui::Context) {
        if let Some(ev) = self.hot_reload_service.recent_events().first() {
            let key = format!(
                "{}:{}:{}",
                ev.timestamp,
                ev.event_type.as_str(),
                ev.path.display()
            );
            if self.last_hot_reload_key.as_deref() != Some(key.as_str()) {
                self.last_hot_reload_key = Some(key);
                self.refresh_all_local_state();
                if let Some(aid) = self.depots_app_id.filter(|&id| id > 0) {
                    self.reload_depots_local(aid);
                }
            }
        }

        while let Ok(ev) = self.rpc_rx.try_recv() {
            match ev {
                RpcUiEvent::AddApp(aid) => {
                    self.restore_from_tray(ctx);
                    self.open_app_in_acquire(aid, ctx);
                }
                RpcUiEvent::OpenFix(aid) => {
                    self.restore_from_tray(ctx);
                    self.open_app_in_fixes(aid, ctx);
                }
                RpcUiEvent::OpenSettings => {
                    self.restore_from_tray(ctx);
                    self.settings_service.sync_autostart_from_disk();
                    self.active_view = ActiveView::Settings;
                }
                RpcUiEvent::RemovedApp(_) => self.refresh_all_local_state(),
                RpcUiEvent::RestoreFromTray => {
                    self.restore_from_tray(ctx);
                }
            }
        }

        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                AppMessage::BridgeProbed {
                    available,
                    version,
                    steam_path,
                } => {
                    self.millennium_connected = available;
                    self.settings_millennium_status = if available {
                        format!(
                            "Connected · Millennium v{} · Steam={}",
                            version.unwrap_or_else(|| "unknown".into()),
                            steam_path.unwrap_or_else(|| "unknown".into())
                        )
                    } else {
                        "Offline · Millennium RPC not running on 127.0.0.1:38495".into()
                    };
                }
                AppMessage::CoverLoaded {
                    app_id,
                    width,
                    height,
                    rgba,
                } => {
                    self.cover_in_flight.remove(&app_id);
                    self.cover_retry_urls.remove(&app_id);
                    self.cover_failed_at.remove(&app_id);
                    let expected = width.saturating_mul(height).saturating_mul(4);
                    if width > 0 && height > 0 && rgba.len() == expected {
                        let img = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                        let tex = ctx.load_texture(
                            format!("cover_{}", app_id),
                            img,
                            TextureOptions::LINEAR,
                        );
                        self.cover_textures.insert(app_id, tex);
                        self.cover_lru.retain(|&id| id != app_id);
                        self.cover_lru.push_back(app_id);
                        while self.cover_lru.len() > 24 {
                            if let Some(oldest) = self.cover_lru.pop_front() {
                                self.cover_textures.remove(&oldest);
                            }
                        }
                    } else {
                        self.cover_failed_at
                            .insert(app_id, std::time::Instant::now());
                    }
                }
                AppMessage::CoverFailed { app_id } => {
                    self.cover_in_flight.remove(&app_id);
                    if let Some(retry_url) = self.cover_retry_urls.remove(&app_id) {
                        self.ensure_cover_requested(app_id, Some(retry_url), ctx);
                    } else {
                        self.cover_failed_at
                            .insert(app_id, std::time::Instant::now());
                    }
                }
                AppMessage::FeaturedLoaded { top, new_releases } => {
                    for item in top.iter().chain(new_releases.iter()) {
                        self.record_known_cover_url(item.app_id, &item.large_capsule_image);
                    }
                    self.featured_top = top;
                    self.featured_new = new_releases;
                    if let Some(aid) = self.acquire_selected_app {
                        if let Some(url) = self.cover_known_urls.get(&aid).cloned() {
                            self.ensure_cover_requested(aid, Some(url), ctx);
                        }
                    }
                }
                AppMessage::SearchFinished { items, error } => {
                    for item in &items {
                        self.record_known_cover_url(item.app_id, &item.icon);
                    }
                    self.search_results = items;
                    self.acquire_tab = AcquireTab::SearchResults;
                    if let Some(e) = error {
                        self.acquire_status = format!("Search error: {}", e);
                    } else {
                        self.acquire_status =
                            format!("Found {} store match(es).", self.search_results.len());
                    }
                }
                AppMessage::AcquireDetailsLoaded { app_id, details } => {
                    if let Some(ref d) = details {
                        if !d.header_image.is_empty() {
                            self.record_known_cover_url(app_id, &d.header_image);
                            self.ensure_cover_requested(app_id, Some(d.header_image.clone()), ctx);
                        }
                    }
                    if self.acquire_selected_app == Some(app_id) {
                        self.acquire_details = details;
                    }
                }
                AppMessage::AcquireSourcesLoaded {
                    app_id,
                    sources,
                    dlc_info,
                } => {
                    if self.acquire_selected_app == Some(app_id) {
                        let avail_n = sources
                            .values()
                            .filter(|s| s.eq_ignore_ascii_case("available"))
                            .count();
                        if self.settings_service.settings.fast_fetch
                            && self.acquire_selected_source.is_none()
                        {
                            self.acquire_selected_source = sources
                                .iter()
                                .find(|(_, st)| st.eq_ignore_ascii_case("available"))
                                .map(|(prov, _)| prov.clone());
                        }
                        self.acquire_sources = sources;
                        self.acquire_dlc_info = dlc_info;
                        self.acquire_status =
                            format!("AppID {}: {} provider(s) available", app_id, avail_n);
                    }
                }
                AppMessage::AcquireProgress { pct, msg } => {
                    self.acquire_progress = pct.clamp(0.0, 1.0);
                    self.acquire_status = msg;
                }
                AppMessage::AcquireInstallFinished {
                    app_id,
                    ok,
                    downloaded_path,
                    source,
                    msg,
                } => {
                    self.acquire_busy = false;
                    let game_title = self.resolve_game_title(app_id);
                    if ok {
                        if let (Some(dl), Some(installer)) =
                            (downloaded_path, self.lua_installer.clone())
                        {
                            if let Some((aid, old_txt, new_txt, added, removed)) =
                                self.check_lua_overwrite_diff(&dl, Some(app_id))
                            {
                                self.overwrite_diff_modal = OverwriteDiffModal {
                                    open: true,
                                    app_id: aid,
                                    source_label: source.clone(),
                                    old_content: old_txt,
                                    new_content: new_txt,
                                    added_lines: added,
                                    removed_lines: removed,
                                    pending_package_path: Some(dl),
                                    cleanup_parent: true,
                                };
                                self.acquire_status = format!(
                                    "Existing {}.lua differs — confirm overwrite to install.",
                                    aid
                                );
                                self.notify(
                                    format!(
                                        "Review changes before overwriting existing {}.lua.",
                                        aid
                                    ),
                                    ToastKind::Info,
                                );
                            } else {
                                let res = installer.install_any(
                                    &dl,
                                    Some(app_id),
                                    false,
                                    Some(&source),
                                    Some(&mut self.settings_service),
                                    Some(&mut self.slssteam_service),
                                );
                                if let Some(parent) = dl.parent() {
                                    let _ = fs::remove_dir_all(parent);
                                }
                                if res.ok {
                                    let msg = format!(
                                        "Installed AppID {} ({} manifest file(s))",
                                        app_id, res.manifest_count
                                    );
                                    self.record_download_history(
                                        app_id,
                                        game_title,
                                        &source,
                                        "Installed",
                                    );
                                    self.acquire_status = msg.clone();
                                    self.notify(msg, ToastKind::Success);
                                    self.refresh_all_local_state();
                                    {
                                        let consent_seen =
                                            self.settings_service.settings.telemetry_consent_seen;
                                        let telemetry_on =
                                            self.settings_service.settings.telemetry_enabled;
                                        let src_copy = source.clone();
                                        if consent_seen && telemetry_on {
                                            thread::spawn(move || {
                                                let _ = AnalyticsService::new().track_event_gated(
                                                    "acquire_install",
                                                    Some(serde_json::json!({ "source": src_copy })),
                                                    consent_seen,
                                                    telemetry_on,
                                                );
                                            });
                                        }
                                    }
                                } else {
                                    let err = res.error.unwrap_or_else(|| "Install failed".into());
                                    self.record_download_history(
                                        app_id,
                                        game_title,
                                        &source,
                                        format!("Failed: {}", err),
                                    );
                                    self.acquire_status = err.clone();
                                    self.notify(err, ToastKind::Warning);
                                }
                            }
                        } else {
                            let err =
                                "Steam installation not available to install downloaded package.";
                            self.record_download_history(app_id, game_title, &source, "No Steam");
                            self.acquire_status = err.into();
                            self.notify(err, ToastKind::Warning);
                        }
                    } else {
                        self.record_download_history(
                            app_id,
                            game_title,
                            &source,
                            format!("Error: {}", msg),
                        );
                        self.acquire_status = msg.clone();
                        self.notify(msg, ToastKind::Warning);
                    }
                }
                AppMessage::DepotInfoLoaded { app_id, info } => {
                    if self.depots_app_id == Some(app_id) {
                        self.depots_info = info;
                        if self.depots_subtab == DepotsSubTab::DownloadPicker {
                            self.rebuild_depot_picks();
                        }
                    }
                }
                AppMessage::DepotDownloadProgress {
                    depot_label,
                    pct,
                    msg,
                } => {
                    if pct >= 0.0 {
                        self.depots_dl_pct = pct.clamp(0.0, 100.0);
                    }
                    let shown_pct = if pct >= 0.0 {
                        pct.clamp(0.0, 100.0)
                    } else {
                        self.depots_dl_pct
                    };
                    self.depots_dl_status =
                        format!("{} · {:.1}% — {}", depot_label, shown_pct, msg);
                }
                AppMessage::DepotDownloadFinished { results } => {
                    self.depots_downloading = false;
                    let ok = results.iter().filter(|r| r.ok).count();
                    let total = results.len();
                    let aid = self.depots_app_id.unwrap_or(0);
                    let gtitle = self.resolve_game_title(aid);
                    self.record_download_history(
                        aid,
                        format!("{} ({} depots)", gtitle, total),
                        "DepotDownloader",
                        format!("{}/{} completed", ok, total),
                    );
                    let first_err = results.iter().find_map(|r| r.error.as_deref());
                    if let Some(err) = first_err {
                        let summary = format!("Downloaded {}/{} depot(s) — {}", ok, total, err);
                        self.depots_dl_status = summary.clone();
                        self.notify(summary, ToastKind::Warning);
                    } else if ok < total {
                        let summary = format!(
                            "Downloaded {}/{} depot(s) — one or more depots failed.",
                            ok, total
                        );
                        self.depots_dl_status = summary.clone();
                        self.notify(summary, ToastKind::Warning);
                    } else {
                        let summary =
                            format!("Completed · {} of {} depot(s) downloaded", ok, total);
                        self.depots_dl_status = summary.clone();
                        self.notify(summary, ToastKind::Success);
                        {
                            let consent_seen = self.settings_service.settings.telemetry_consent_seen;
                            let telemetry_on = self.settings_service.settings.telemetry_enabled;
                            if consent_seen && telemetry_on {
                                thread::spawn(move || {
                                    let _ = AnalyticsService::new().track_event_gated(
                                        "depot_download",
                                        Some(serde_json::json!({ "count": ok })),
                                        consent_seen,
                                        telemetry_on,
                                    );
                                });
                            }
                        }
                    }
                    self.rebuild_depot_picks();
                }
                AppMessage::FixListingsLoaded { games, tags } => {
                    self.fixes_listings = games;
                    self.fixes_tags = tags;
                }
                AppMessage::FixesLoaded { app_id, fixes } => {
                    if self.fixes_current_app == Some(app_id) {
                        self.fixes_selected_row = (!fixes.is_empty()).then_some(0);
                        self.fixes_items = fixes;
                    }
                }
                AppMessage::FixProgress { pct, msg } => {
                    self.fixes_progress = pct.clamp(0.0, 1.0);
                    self.status_text = msg;
                }
                AppMessage::FixActionFinished {
                    app_id,
                    ok,
                    downloaded_manifest,
                    fix_id,
                    msg,
                } => {
                    self.fixes_busy = false;
                    let mut final_ok = ok;
                    let mut final_msg = msg;
                    if ok {
                        if let Some(dl) = downloaded_manifest {
                            if let Some(installer) = self.lua_installer.clone() {
                                let res = installer.install_any(
                                    &dl,
                                    Some(app_id),
                                    true,
                                    Some(&format!("fix:{}", fix_id)),
                                    Some(&mut self.settings_service),
                                    Some(&mut self.slssteam_service),
                                );
                                if let Some(p) = dl.parent() {
                                    let _ = fs::remove_dir_all(p);
                                }
                                self.refresh_all_local_state();
                                if !res.ok {
                                    final_ok = false;
                                    final_msg = res.error.unwrap_or_else(|| {
                                        "Failed to install fix manifest.".into()
                                    });
                                }
                            } else {
                                if let Some(p) = dl.parent() {
                                    let _ = fs::remove_dir_all(p);
                                }
                                final_ok = false;
                                final_msg =
                                    "Steam installation not detected — fix manifest not installed."
                                        .into();
                            }
                        }
                    }
                    if ok {
                        self.applied_fix_index.rebuild_index(&self.steam_service);
                    }
                    let gtitle = self.resolve_game_title(app_id);
                    self.record_download_history(
                        app_id,
                        format!("{} (Fix {})", gtitle, fix_id),
                        "GameFix",
                        if final_ok { "Applied" } else { "Failed" },
                    );
                    self.notify(
                        final_msg,
                        if final_ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::ToolProgress { tool_key, pct, msg } => {
                    self.tool_progress.insert(tool_key, pct.clamp(0.0, 1.0));
                    self.status_text = msg;
                }
                AppMessage::ToolFinished { tool_key, ok, msg } => {
                    self.tool_busy.insert(tool_key, false);
                    self.cached_tool_statuses.clear();
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::AuthCompleted { ok, msg } => {
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                    if ok {
                        self.fetch_usage_async(ctx);
                    }
                }
                AppMessage::UsageLoaded(txt) => {
                    self.settings_usage_text = txt;
                }
                AppMessage::HubcapChecked { ok, msg } => {
                    self.settings_hubcap_status = msg.clone();
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::DonatedKeys(count) => {
                    let m = if count > 0 {
                        format!("Submitted {} new decryption key(s) from config.vdf.", count)
                    } else {
                        "No new decryption keys to submit.".into()
                    };
                    self.notify(m, ToastKind::Info);
                }
                AppMessage::SteamRestarted { ok, msg } => {
                    if ok {
                        self.steam_bridge_service.refresh_steam_detection();
                    }
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Info
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::FilesPickedForIngest(files) => {
                    self.install_dropped_paths(&files);
                }
                AppMessage::FolderPickedForDepotDest(dir) => {
                    self.depots_dest_dir = dir.to_string_lossy().to_string();
                }
                AppMessage::FolderPickedForSteamOverride(dir) => {
                    self.settings_service.settings.steam_path_override =
                        dir.to_string_lossy().to_string();
                }
                AppMessage::FilePickedForWorkshop(f) => {
                    self.slssteam_service.config.workshop_path = f.to_string_lossy().to_string();
                }
                AppMessage::FolderPickedForAutoCrack(dir) => {
                    let aid = self.depots_app_id.unwrap_or(0);
                    let res = if aid > 0 {
                        self.steam_auto_crack.launch_for_app(aid, &dir)
                    } else {
                        self.steam_auto_crack.launch_for_folder(&dir)
                    };
                    match res {
                        Ok(m) => self.notify(m, ToastKind::Info),
                        Err(e) => self.notify(e, ToastKind::Warning),
                    }
                }
                AppMessage::FilePickedForStubRemover(exe) => {
                    self.tool_busy.insert("stub_remover".to_string(), true);
                    self.status_text = format!("Running StubRemover on {}…", exe.display());
                    let mgr = self.tools_manager.clone();
                    let tx = self.msg_tx.clone();
                    let ctx_c = ctx.clone();
                    thread::spawn(move || {
                        let (ok, msg) = match mgr.launch_stub_remover(&exe) {
                            Ok(m) => (true, m),
                            Err(m) => (false, m),
                        };
                        let _ = tx.send(AppMessage::ToolFinished {
                            tool_key: "stub_remover".to_string(),
                            ok,
                            msg,
                        });
                        ctx_c.request_repaint();
                    });
                }
                AppMessage::UpdateCheckFinished(chk) => {
                    self.settings_update_status = match &chk.latest_commit {
                        Some(c) => format!("{}  ·  {}", chk.summary, c),
                        None => chk.summary.clone(),
                    };
                    if chk.available {
                        let badge = match (&chk.remote_version, chk.commits_behind) {
                            (Some(v), _) if v != VERSION => format!("⬆ Update v{}", v),
                            (_, b) if b > 0 => format!("⬆ Update (+{})", b),
                            _ => "⬆ Update".into(),
                        };
                        let mut tip = chk.summary.clone();
                        if let Some(c) = &chk.latest_commit {
                            tip.push_str(&format!("\nLatest: {}", c));
                        }
                        tip.push_str("\nClick to pull latest files from Git and update.");
                        self.update_banner = UpdateBannerState::Available {
                            badge,
                            tooltip: tip,
                        };
                    } else if !matches!(
                        self.update_banner,
                        UpdateBannerState::Updating { .. }
                            | UpdateBannerState::ReadyToRestart { .. }
                    ) {
                        self.update_banner = UpdateBannerState::Hidden;
                    }
                }
                AppMessage::UpdateProgress { pct, msg } => {
                    self.status_text = msg.clone();
                    self.settings_update_status = msg.clone();
                    self.update_banner = UpdateBannerState::Updating {
                        pct: pct.clamp(0.0, 1.0),
                        step: msg,
                    };
                }
                AppMessage::UpdateApplyFinished(outcome) => match outcome {
                    Ok(res) => {
                        let summary = format!(
                            "Updated to v{} ({}) — click ↻ Restart in the top-right corner.",
                            res.new_version, res.commit_info
                        );
                        self.settings_update_status = summary.clone();
                        self.notify(summary.clone(), ToastKind::Success);
                        self.update_banner = UpdateBannerState::ReadyToRestart {
                            new_version: res.new_version,
                            exe_path: res.updated_exe,
                            summary,
                        };
                    }
                    Err(err) => {
                        self.settings_update_status = format!("Error: {}", err);
                        self.notify(format!("Update failed: {}", err), ToastKind::Warning);
                        self.update_banner = UpdateBannerState::Available {
                            badge: "⬆ Retry Update".into(),
                            tooltip: format!("Last attempt failed: {}\nClick to retry.", err),
                        };
                    }
                },
                AppMessage::FolderPickedForUpdateRepo(dir) => {
                    self.settings_service.settings.update_repo_dir =
                        dir.to_string_lossy().to_string();
                }
                AppMessage::TelemetryEndpointTested { ok, msg } => {
                    self.settings_telemetry_status = msg.clone();
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::FolderPickedForDiagExport(dir) => {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    let dest = dir.join(format!("luatools-diagnostics-{}.md", ts));
                    match self.telemetry_service.export_report(&dest) {
                        Ok(p) => self.notify(
                            format!("Exported diagnostic report to {}", p.display()),
                            ToastKind::Success,
                        ),
                        Err(e) => self.notify(
                            format!("Failed to export diagnostic report: {}", e),
                            ToastKind::Warning,
                        ),
                    }
                }
                AppMessage::WiredProgress { pct, msg } => {
                    self.wired_progress = pct.clamp(0.0, 1.0);
                    self.wired_status_text = msg.clone();
                    self.status_text = msg;
                }
                AppMessage::WiredFinished { ok, msg } => {
                    self.wired_busy = false;
                    self.wired_status_text = msg.clone();
                    self.refresh_all_local_state();
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::BridgeActionFinished { ok, msg } => {
                    self.bridge_busy = false;
                    self.bridge_status_msg = msg.clone();
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::GameIntegrityChecked {
                    app_id: _,
                    checked: _,
                    ok,
                    msg,
                } => {
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::FilePickedForGameInjectionSo(path) => {
                    self.integration_modal.proc_lib_input = path.to_string_lossy().to_string();
                }
                AppMessage::LaunchOptionsFinished {
                    app_id,
                    ok,
                    entries,
                    message,
                    warn,
                } => {
                    if self.launch_options_modal.app_id == app_id {
                        self.launch_options_modal.busy = false;
                        self.launch_options_modal.feedback = message.clone();
                        if let Some(new_entries) = entries {
                            self.launch_options_modal.selected_idx =
                                (!new_entries.is_empty()).then_some(0);
                            self.launch_options_modal.entries = new_entries;
                        }
                        self.launch_options_modal.steam_running =
                            crate::services::launch_mod_store::is_any_steam_running();
                    }
                    self.notify(
                        message,
                        if ok && !warn {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::ShowWindow => {
                    self.restore_from_tray(ctx);
                }
                AppMessage::HideWindow => {
                    self.hide_to_tray(ctx);
                }
                AppMessage::OpenSettings => {
                    self.restore_from_tray(ctx);
                    self.settings_service.sync_autostart_from_disk();
                    self.active_view = ActiveView::Settings;
                }
                AppMessage::RestartSteamRequested => {
                    self.trigger_restart_steam(ctx);
                }
                AppMessage::QuitRequested => {
                    self.request_quit(ctx);
                }
            }
        }
    }

    pub fn can_hide_to_tray(&self) -> bool {
        self.tray_service.is_tray_available()
    }

    pub fn should_intercept_close_to_tray(&self, cli_tray_locked: bool) -> bool {
        if self.quit_requested {
            return false;
        }
        let want_close_to_tray = cli_tray_locked || self.tray_locked || self.settings_service.settings.close_to_tray;
        want_close_to_tray && self.can_hide_to_tray()
    }

    pub fn restore_from_tray(&mut self, ctx: &egui::Context) {
        self.suppress_minimize_hook_frames = 6;
        self.prev_viewport_minimized = false;
        self.tray_service.set_visibility(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    pub fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if self.can_hide_to_tray() {
            self.tray_service.set_visibility(false);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if !self.tray_hide_notified {
                self.tray_hide_notified = true;
                self.tray_service.show_notification(
                    "LuaTools Linux",
                    "LuaTools is running in the system tray. Use the tray icon to restore or quit.",
                );
            }
        } else {
            // Safe fallback when no desktop tray host exists: iconify in taskbar rather than hiding headless
            self.tray_service.set_visibility(true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    pub fn request_quit(&mut self, ctx: &egui::Context) {
        self.quit_requested = true;
        self.tray_service.cleanup();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub fn handle_minimize_to_tray_hook(&mut self, ctx: &egui::Context) {
        let cur_min = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if self.suppress_minimize_hook_frames > 0 {
            self.suppress_minimize_hook_frames -= 1;
            self.prev_viewport_minimized = cur_min;
            return;
        }
        let just_minimized = cur_min && !self.prev_viewport_minimized;
        self.prev_viewport_minimized = cur_min;

        if just_minimized
            && self.tray_service.is_visible()
            && self.settings_service.settings.minimize_to_tray
            && self.can_hide_to_tray()
        {
            self.hide_to_tray(ctx);
        }
    }

    fn handle_shortcuts_and_drops(&mut self, ctx: &egui::Context) {
        let mut trigger_quit = false;
        ctx.input_mut(|i| {
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::K))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::L))
            {
                self.focus_command_next = true;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num1)) {
                self.active_view = ActiveView::Overview;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num2)) {
                self.active_view = ActiveView::Acquire;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num3)) {
                self.active_view = ActiveView::Library;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num4)) {
                self.active_view = ActiveView::Depots;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num5)) {
                self.active_view = ActiveView::Fixes;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num6))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::NONE, Key::F1))
            {
                self.active_view = ActiveView::Guide;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num7))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Comma))
            {
                self.settings_service.sync_autostart_from_disk();
                self.active_view = ActiveView::Settings;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Q)) {
                trigger_quit = true;
            }
        });
        if trigger_quit {
            if self.tray_locked {
                self.hide_to_tray(ctx);
                self.notify(
                    "LuaTools is locked to tray. Use Tray Menu > Quit to exit.",
                    ToastKind::Info,
                );
            } else {
                self.request_quit(ctx);
            }
        }

        let (had_raw_drops, dropped): (bool, Vec<PathBuf>) = ctx.input(|i| {
            let has_any = !i.raw.dropped_files.is_empty();
            let valid = i
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| {
                            matches!(e.to_ascii_lowercase().as_str(), "lua" | "manifest" | "zip")
                        })
                        .unwrap_or(false)
                })
                .collect();
            (has_any, valid)
        });
        if !dropped.is_empty() {
            self.install_dropped_paths(&dropped);
        } else if had_raw_drops {
            self.notify(
                "Unsupported file type — drop .lua, .manifest, or .zip packages.",
                ToastKind::Warning,
            );
        }
    }

    fn execute_quick_command(&mut self, ctx: &egui::Context) {
        let raw = self.command_input.trim().to_string();
        if raw.is_empty() {
            return;
        }
        self.command_input.clear();

        if raw.to_ascii_lowercase().starts_with("luatools://") {
            self.handle_protocol_url(&raw, ctx);
            return;
        }
        let unquoted = raw.trim_matches(|c| c == '"' || c == '\'');
        let p = Path::new(unquoted);
        if p.is_file() {
            self.install_dropped_paths(&[p.to_path_buf()]);
            return;
        }
        if let Some(aid) = SteamLinkParser::app_id_from(&raw) {
            self.open_app_in_acquire(aid, ctx);
            self.status_text = format!("Opened AppID {} in Discover", aid);
            return;
        }

        self.acquire_search_query = raw.clone();
        self.active_view = ActiveView::Acquire;
        self.execute_store_search(&raw, ctx);
    }

    /// Renders the complete UI shell into `ctx` (callable both from `eframe::App::update` and headless layout tests).
    pub fn render_shell(&mut self, ctx: &egui::Context) {
        // Release decoded guide screenshot textures as soon as the user leaves Help
        if self.active_view != ActiveView::Guide
            && self.guide_zoom_screenshot.is_none()
            && !self.guide_textures.is_empty()
        {
            self.guide_textures.clear();
        }
        // Lazy-load remote Steam catalog & Game Fixes catalog on first visit (keeps startup fast & low-RAM)
        if self.active_view == ActiveView::Acquire
            && !self.featured_requested
            && self.featured_top.is_empty()
            && self.featured_new.is_empty()
        {
            self.featured_requested = true;
            self.fetch_featured_async(ctx);
        }
        if self.active_view == ActiveView::Fixes
            && !self.fixes_requested
            && self.fixes_listings.is_empty()
        {
            self.fixes_requested = true;
            self.fetch_fix_listings_async(ctx);
        }

        if self.active_view == ActiveView::Depots && self.depots_app_id.is_none() {
            if let Some((first_aid, _)) = self.recent_installed.first().cloned() {
                self.open_game_in_depots(first_aid, ctx);
            } else if let Some(first_row) = self.library_rows.first() {
                self.open_game_in_depots(first_row.app_id, ctx);
            }
        }

        // 1. Top Bar — Strict 3-Column StripBuilder (mathematically zero overlap at any width)
        //    Houses Brand (left), Search bar (center), and Steam status + corner Git Update button (top-right).
        let mut run_omnibox = false;
        let mut click_rescan = false;
        let mut click_open_steam_setup = false;
        let mut click_start_update = false;
        let mut click_launch_bridge = false;
        let mut click_kill_steam = false;
        let mut click_relaunch_exe: Option<PathBuf> = None;

        let bridge_st = self.steam_bridge_service.status();
        let steam_online = self.steam_service.is_available();
        let steam_running = bridge_st.steam_pid.is_some();
        let steam_ready = self.steam_sls_installed
            || bridge_st.integration_lib_installed
            || bridge_st.launcher_installed
            || bridge_st.system_preload_enabled;

        let base_right_w = if steam_running { 276.0 } else { 188.0 };
        let right_col_w = if matches!(self.update_banner, UpdateBannerState::Hidden) {
            base_right_w
        } else {
            base_right_w + 132.0
        };

        egui::TopBottomPanel::top("top_command_bar")
            .exact_height(56.0)
            .frame(
                Frame::new()
                    .fill(Palette::BG_VOID)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                    .inner_margin(Margin::symmetric(16, 0)),
            )
            .show(ctx, |ui| {
                StripBuilder::new(ui)
                    .size(Size::exact(208.0))
                    .size(Size::remainder().at_least(160.0))
                    .size(Size::exact(right_col_w))
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .horizontal(|mut strip| {
                        // Left: Cut-Paper Rem Medallion + Rich Display Title
                        strip.cell(|ui| {
                            ui.spacing_mut().item_spacing.x = 9.0;
                            draw_rem_medallion(ui, 30.0);
                            ui.label(
                                RichText::new("LuaTools")
                                    .font(display_font(18.5))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(VERSION)
                                    .font(FontId::new(11.0, FontFamily::Monospace))
                                    .color(Palette::TEXT_MUTED),
                            );
                        });

                        // Center: Single Clean Bounded Search Box + Search Button
                        strip.cell(|ui| {
                            let avail = ui.available_width();
                            let input_w = (avail - 78.0).clamp(120.0, 480.0);
                            let pad_left = ((avail - (input_w + 68.0)) * 0.5).max(0.0);
                            if pad_left > 2.0 {
                                ui.add_space(pad_left);
                            }
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let resp = ui.add_sized(
                                [input_w, 32.0],
                                TextEdit::singleline(&mut self.command_input)
                                    .margin(Margin::symmetric(10, 7))
                                    .hint_text("Search any Steam game or paste a link…  (Ctrl+K)"),
                            );
                            if self.focus_command_next {
                                resp.request_focus();
                                self.focus_command_next = false;
                            }
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if enter || primary_button(ui, "Search").clicked() {
                                run_omnibox = true;
                            }
                        });

                        // Right: Steam Launch Pill + Optional Stop Steam + Corner Update Button
                        strip.cell(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;

                                let (lbl, bg, fg, dot_col, tip) = if steam_ready && steam_running {
                                    (
                                        "Restart Steam",
                                        Palette::GO_BG,
                                        Palette::GO_TEXT,
                                        Palette::GO_TEXT,
                                        "Restart Steam so newly added games and changes take effect.",
                                    )
                                } else if steam_ready {
                                    (
                                        "Launch Steam",
                                        Palette::BLUE_PRIMARY,
                                        Palette::TEXT_PRIMARY,
                                        Palette::BLUE_LIGHT,
                                        "Launch Steam with LuaTools game unlocker active.",
                                    )
                                } else if steam_online {
                                    (
                                        "Setup Steam",
                                        Palette::WARN_BG,
                                        Palette::WARN_TEXT,
                                        Palette::WARN_TEXT,
                                        "Steam was found, but the game unlocker isn't installed yet. Click for 1-click setup.",
                                    )
                                } else {
                                    (
                                        "Rescan Steam",
                                        Palette::WARN_BG,
                                        Palette::WARN_TEXT,
                                        Palette::WARN_TEXT,
                                        "Steam not detected. Click to rescan or set your Steam folder in Settings.",
                                    )
                                };

                                let g = ui.painter().layout_no_wrap(
                                    lbl.to_string(),
                                    semibold_font(11.5),
                                    fg,
                                );
                                let sz = Vec2::new(g.size().x + 34.0, 28.0);
                                let (rect, pill_resp) = ui.allocate_exact_size(sz, Sense::click());
                                if pill_resp.hovered() {
                                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                }
                                if ui.is_rect_visible(rect) {
                                    ui.painter().rect_filled(
                                        rect.translate(Vec2::new(0.0, 1.5)),
                                        CornerRadius::same(4),
                                        Palette::BG_PANEL_LO,
                                    );
                                    ui.painter()
                                        .rect_filled(rect, CornerRadius::same(4), bg);
                                    let dot_pos = Pos2::new(rect.left() + 12.0, rect.center().y);
                                    if steam_ready && steam_running {
                                        ui.painter().circle_filled(dot_pos, 3.6, dot_col);
                                    } else {
                                        ui.painter().circle_stroke(
                                            dot_pos,
                                            3.4,
                                            Stroke::new(1.5_f32, dot_col),
                                        );
                                    }
                                    ui.painter().galley(
                                        Pos2::new(
                                            rect.left() + 21.0,
                                            rect.center().y - g.size().y * 0.5,
                                        ),
                                        g,
                                        fg,
                                    );
                                }
                                if pill_resp.on_hover_text(tip).clicked() {
                                    if steam_ready {
                                        click_launch_bridge = true;
                                    } else if steam_online {
                                        click_open_steam_setup = true;
                                    } else {
                                        click_rescan = true;
                                    }
                                }

                                // Only show Stop Steam in the top bar while Steam is running
                                if steam_running {
                                    let kill_btn = egui::Button::new(
                                        RichText::new("Stop Steam")
                                            .font(semibold_font(11.5))
                                            .color(Palette::DANGER_TEXT),
                                    )
                                    .fill(Palette::DANGER_BG)
                                    .stroke(Stroke::new(1.0_f32, Palette::DANGER_BORDER))
                                    .corner_radius(CornerRadius::same(4))
                                    .min_size(Vec2::new(82.0, 28.0));
                                    let kill_resp = ui.add(kill_btn).on_hover_text(
                                        "Close all running Steam processes cleanly.",
                                    );
                                    if kill_resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    if kill_resp.clicked() {
                                        click_kill_steam = true;
                                    }
                                }

                                match &self.update_banner {
                                    UpdateBannerState::Hidden => {}
                                    UpdateBannerState::Available { badge, tooltip } => {
                                        let btn = egui::Button::new(
                                            RichText::new(badge)
                                                .font(semibold_font(11.5))
                                                .color(Palette::TEXT_PRIMARY),
                                        )
                                        .fill(Palette::BLUE_PRIMARY)
                                        .stroke(Stroke::new(1.0_f32, Palette::BLUE_BORDER))
                                        .corner_radius(CornerRadius::same(4))
                                        .min_size(Vec2::new(104.0, 28.0));
                                        let resp = ui.add(btn).on_hover_text(tooltip);
                                        if resp.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        if resp.clicked() {
                                            click_start_update = true;
                                        }
                                    }
                                    UpdateBannerState::Updating { pct, step } => {
                                        let lbl = format!("⟳ {:>2.0}%", pct * 100.0);
                                        let btn = egui::Button::new(
                                            RichText::new(lbl)
                                                .font(semibold_font(11.5))
                                                .color(Palette::BLUE_LIGHT),
                                        )
                                        .fill(Palette::BLUE_SUBTLE)
                                        .stroke(Stroke::new(1.0_f32, Palette::BLUE_BORDER))
                                        .corner_radius(CornerRadius::same(4))
                                        .min_size(Vec2::new(76.0, 28.0));
                                        ui.add_enabled(false, btn).on_disabled_hover_text(step);
                                    }
                                    UpdateBannerState::ReadyToRestart {
                                        new_version,
                                        exe_path,
                                        summary,
                                    } => {
                                        let btn = egui::Button::new(
                                            RichText::new(format!("↻ Restart v{}", new_version))
                                                .font(semibold_font(11.5))
                                                .color(Palette::TEXT_PRIMARY),
                                        )
                                        .fill(Palette::BLUE_HOVER)
                                        .stroke(Stroke::new(1.0_f32, Palette::BLUE_LIGHT))
                                        .corner_radius(CornerRadius::same(4))
                                        .min_size(Vec2::new(112.0, 28.0));
                                        let resp = ui.add(btn).on_hover_text(summary);
                                        if resp.hovered() {
                                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                        }
                                        if resp.clicked() {
                                            click_relaunch_exe = Some(exe_path.clone());
                                        }
                                    }
                                }
                            });
                        });
                    });
            });

        if run_omnibox {
            self.execute_quick_command(ctx);
        }
        if click_rescan {
            self.detect_steam_and_sync();
        }
        if click_open_steam_setup {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Integrations;
        }
        if click_launch_bridge {
            self.launch_steam_with_bridge(ctx);
        }
        if click_kill_steam {
            self.trigger_kill_steam(ctx);
        }
        if click_start_update {
            self.trigger_git_self_update(ctx);
        }
        if let Some(exe) = click_relaunch_exe {
            match UpdateService::relaunch_binary(&exe) {
                Ok(()) => self.request_quit(ctx),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }

        // 3. Left Sidebar Rail — Calm, Clean, Spacious
        egui::SidePanel::left("left_nav_rail")
            .exact_width(204.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(Palette::BG_VOID)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                    .inner_margin(Margin::ZERO),
            )
            .show(ctx, |ui| {
                // Dock Settings & Account quietly at the bottom of the rail so they never collide
                egui::TopBottomPanel::bottom("nav_bottom_section")
                    .frame(
                        Frame::new()
                            .fill(Palette::BG_VOID)
                            .inner_margin(Margin::symmetric(0, 10)),
                    )
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        if nav_rail_button(
                            ui,
                            "Settings",
                            None,
                            self.active_view == ActiveView::Settings,
                            204.0,
                        )
                        .on_hover_text(
                            "Steam folder path, 1-click SLSsteam setup, Discord/Hubcap account, Cloud Save Redirect, and tools (Ctrl+7)",
                        )
                        .clicked()
                        {
                            self.settings_service.sync_autostart_from_disk();
                            self.active_view = ActiveView::Settings;
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            let acct = if self.auth_service.is_signed_in() {
                                format!("● {}", self.auth_service.display_name())
                            } else {
                                "○ Guest mode".into()
                            };
                            let acct_resp = ui
                                .add(
                                    egui::Label::new(RichText::new(acct).size(11.5).color(
                                        if self.auth_service.is_signed_in() {
                                            Palette::BLUE_LIGHT
                                        } else {
                                            Palette::TEXT_MUTED
                                        },
                                    ))
                                    .sense(Sense::click()),
                                )
                                .on_hover_text("Open Account & Providers settings");
                            if acct_resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if acct_resp.clicked() {
                                self.active_view = ActiveView::Settings;
                                self.settings_subtab = SettingsSubTab::Account;
                            }
                        });
                    });

                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(
                        RichText::new("GAMES")
                            .font(semibold_font(10.5))
                            .color(Palette::TEXT_MUTED),
                    );
                });
                ui.add_space(6.0);

                let lua_n = self
                    .library_rows
                    .iter()
                    .filter(|r| r.has_lua)
                    .count()
                    .max(self.recent_installed.len());
                let primary_items = [
                    (
                        "Home",
                        None,
                        ActiveView::Overview,
                        "Drop .lua/.zip/.manifest files, view setup status, and open recently added games (Ctrl+1)",
                    ),
                    (
                        "Add Games",
                        None,
                        ActiveView::Acquire,
                        "Search Steam games, install .lua manifests from cloud providers, and generate DLCs (Ctrl+2)",
                    ),
                    (
                        "My Games",
                        Some(lua_n),
                        ActiveView::Library,
                        "Manage all added and installed games: Integrate, Files & DLC, Edit .lua, Launch Options, and Fixes (Ctrl+3)",
                    ),
                    (
                        "Game Fixes",
                        None,
                        ActiveView::Fixes,
                        "Apply or revert verified DRM and online patches with SHA-256 backup (Ctrl+5)",
                    ),
                ];
                for (lbl, count, view, tip) in primary_items {
                    if nav_rail_button(ui, lbl, count, self.active_view == view, 204.0)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.active_view = view;
                    }
                }

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(
                        RichText::new("TOOLS")
                            .font(semibold_font(10.5))
                            .color(Palette::TEXT_MUTED),
                    );
                });
                ui.add_space(6.0);

                let secondary_items = [
                    (
                        "Depots & Vault",
                        None,
                        ActiveView::Depots,
                        "Toggle DLCs, switch saved .lua builds in LuaVault, pin manifest versions, and download depots (Ctrl+4)",
                    ),
                    (
                        "Help",
                        None,
                        ActiveView::Guide,
                        "Illustrated user guide, common tasks reference, and troubleshooting tips (Ctrl+6 / F1)",
                    ),
                ];
                for (lbl, count, view, tip) in secondary_items {
                    if nav_rail_button(ui, lbl, count, self.active_view == view, 204.0)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.active_view = view;
                    }
                }
            });

        // 4. Main Central Desk Canvas (generous 24px margin)
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(Palette::BG_BASE)
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show(ctx, |ui| match self.active_view {
                ActiveView::Overview => self.render_overview(ui),
                ActiveView::Acquire => self.render_acquire(ui),
                ActiveView::Library => self.render_library(ui),
                ActiveView::Depots => self.render_depots(ui),
                ActiveView::Fixes => self.render_fixes(ui),
                ActiveView::Guide => self.render_guide(ui),
                ActiveView::Settings => self.render_settings(ui),
            });

        // 5. Tactile Drag-and-Drop Overlay
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering_files {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(
                screen,
                CornerRadius::ZERO,
                Color32::from_rgba_unmultiplied(12, 16, 26, 215),
            );
            painter.rect_stroke(
                screen.shrink(20.0),
                CornerRadius::same(8),
                Stroke::new(2.0_f32, Palette::BLUE_BORDER),
                egui::StrokeKind::Inside,
            );
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drop .lua, .manifest, or .zip package to install",
                display_font(22.0),
                Palette::TEXT_PRIMARY,
            );
        }

        // 6. Top-Right Dismissible Toast Notification (never hinders low-placed text!)
        self.render_top_right_toast(ctx);

        // 7. Modal Windows
        self.render_modals(ctx);
    }

    fn render_top_right_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = self.active_toast.clone() else {
            return;
        };
        let elapsed = toast.created_at.elapsed().as_secs_f32();
        if elapsed >= toast.duration_secs {
            self.active_toast = None;
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(120));

        let accent = match toast.kind {
            ToastKind::Info => Palette::BLUE_LIGHT,
            ToastKind::Success => Palette::GO_TEXT,
            ToastKind::Warning => Palette::WARN_TEXT,
        };

        let mut dismiss = false;
        let max_toast_w = (ctx.screen_rect().width() - 260.0).clamp(280.0, 680.0);
        egui::Area::new(egui::Id::new("top_toast_area"))
            .anchor(egui::Align2::CENTER_TOP, Vec2::new(96.0, 64.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                ui.set_max_width(max_toast_w);
                let inner = Frame::new()
                    .fill(Palette::BG_ELEVATED)
                    .stroke(Stroke::new(1.0_f32, accent))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 10,
                        spread: 0,
                        color: Color32::from_black_alpha(140),
                    })
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(Margin::symmetric(14, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            let (bar, _) =
                                ui.allocate_exact_size(Vec2::new(3.5, 18.0), Sense::hover());
                            ui.painter().rect_filled(bar, CornerRadius::same(2), accent);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&toast.text)
                                        .size(12.5)
                                        .color(Palette::TEXT_PRIMARY),
                                )
                                .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                            ui.add_space(4.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("x")
                                            .font(semibold_font(12.5))
                                            .color(Palette::TEXT_MUTED),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text("Dismiss notification")
                                .clicked()
                            {
                                dismiss = true;
                            }
                        });
                    });
                if inner.response.interact(Sense::click()).clicked() {
                    dismiss = true;
                }
            });

        if dismiss {
            self.active_toast = None;
        }
    }

    fn drive_qa_screenshots(&mut self, ctx: &egui::Context) {
        // Save any screenshot returned by the backend in this frame
        let captured_img: Option<Arc<ColorImage>> = ctx.input(|i| {
            i.events.iter().find_map(|ev| match ev {
                egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                _ => None,
            })
        });

        let Some(ref mut qa) = self.qa_capture else {
            return;
        };

        if let (Some(img), Some(fname)) = (captured_img, qa.pending_save.take()) {
            let w = img.width() as u32;
            let h = img.height() as u32;
            let mut raw = Vec::with_capacity((w * h * 4) as usize);
            for px in &img.pixels {
                raw.extend_from_slice(&px.to_array());
            }
            let out_path = qa.out_dir.join(fname);
            let _ = image::save_buffer(&out_path, &raw, w, h, image::ColorType::Rgba8);
            qa.step += 1;
            qa.frames_waited = 0;
        }

        struct QaShotStep {
            fname: &'static str,
            view: ActiveView,
            s_tab: Option<SettingsSubTab>,
            d_tab: Option<DepotsSubTab>,
            resize: Option<[f32; 2]>,
        }

        let steps: &[QaShotStep] = &[
            QaShotStep {
                fname: "01_home.png",
                view: ActiveView::Overview,
                s_tab: None,
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "02_discover.png",
                view: ActiveView::Acquire,
                s_tab: None,
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "03_library.png",
                view: ActiveView::Library,
                s_tab: None,
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "04_depots_vault.png",
                view: ActiveView::Depots,
                s_tab: None,
                d_tab: Some(DepotsSubTab::Declarations),
                resize: None,
            },
            QaShotStep {
                fname: "05_depots_editor.png",
                view: ActiveView::Depots,
                s_tab: None,
                d_tab: Some(DepotsSubTab::LiveEditor),
                resize: None,
            },
            QaShotStep {
                fname: "06_fixes.png",
                view: ActiveView::Fixes,
                s_tab: None,
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "07_settings_general.png",
                view: ActiveView::Settings,
                s_tab: Some(SettingsSubTab::General),
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "08_settings_updates.png",
                view: ActiveView::Settings,
                s_tab: Some(SettingsSubTab::UpdatesAndTools),
                d_tab: None,
                resize: None,
            },
            QaShotStep {
                fname: "09_narrow_1060x660.png",
                view: ActiveView::Overview,
                s_tab: None,
                d_tab: None,
                resize: Some([1060.0, 660.0]),
            },
        ];

        if qa.step >= steps.len() {
            self.request_quit(ctx);
            return;
        }

        let cur = &steps[qa.step];
        self.active_view = cur.view;
        if let Some(st) = cur.s_tab {
            self.settings_subtab = st;
        }
        if let Some(dt) = cur.d_tab {
            self.depots_subtab = dt;
        }
        if qa.frames_waited == 0 {
            if let Some(sz) = cur.resize {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(sz[0], sz[1])));
            }
        }

        qa.frames_waited += 1;
        if qa.frames_waited == 4 && qa.pending_save.is_none() {
            qa.pending_save = Some(cur.fname.to_string());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

use std::sync::Arc;

impl eframe::App for LuaToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_messages(ctx);
        self.handle_minimize_to_tray_hook(ctx);
        self.handle_shortcuts_and_drops(ctx);
        self.render_shell(ctx);
        self.drive_qa_screenshots(ctx);
    }
}

#[cfg(test)]
mod tray_lifecycle_tests {
    use super::*;

    #[test]
    fn test_close_and_minimize_to_tray_never_leave_headless_window() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());

        // 1. Without an active desktop tray host, close-to-tray MUST NOT intercept close
        app.tray_service.set_tray_available_for_test(false);
        app.settings_service.settings.close_to_tray = true;
        assert!(!app.should_intercept_close_to_tray(false));
        assert!(!app.should_intercept_close_to_tray(true));

        // Calling hide_to_tray without a tray host keeps tray_service visible (taskbar minimize fallback)
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.hide_to_tray(ctx);
        });
        assert!(app.tray_service.is_visible());

        // 2. With an active desktop tray host, close-to-tray intercepts close and hides to tray
        app.tray_service.set_tray_available_for_test(true);
        assert!(app.should_intercept_close_to_tray(false));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.hide_to_tray(ctx);
        });
        assert!(!app.tray_service.is_visible());

        // 3. Restoring from tray marks window visible and arms minimize suppression frames
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.restore_from_tray(ctx);
        });
        assert!(app.tray_service.is_visible());
        assert!(app.suppress_minimize_hook_frames > 0);

        // 4. Explicit Quit (Tray -> Quit or Ctrl+Q) always bypasses close-to-tray interception
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            app.request_quit(ctx);
        });
        assert!(app.quit_requested);
        assert!(!app.should_intercept_close_to_tray(true));
    }

    #[test]
    fn test_ctrl_q_with_tray_locked_hides_to_tray_instead_of_terminating() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());
        app.tray_locked = true;
        app.tray_service.set_tray_available_for_test(true);

        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Q,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        });

        let _ = ctx.run(input, |ctx| {
            app.handle_shortcuts_and_drops(ctx);
        });

        assert!(!app.quit_requested, "Ctrl+Q must not terminate when tray_locked is active");
        assert!(!app.tray_service.is_visible(), "Window should be hidden to tray");
        assert!(
            app.active_toast
                .as_ref()
                .map(|t| t.text.contains("LuaTools is locked to tray. Use Tray Menu > Quit to exit."))
                .unwrap_or(false),
            "Expected tray lock toast notification"
        );
    }
}
