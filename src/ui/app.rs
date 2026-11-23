//! Main `LuaToolsApp` state coordinator & Papercut Monotone-Dark window shell (`eframe::App`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, CursorIcon, FontFamily, FontId, Frame, Key,
    KeyboardShortcut, Layout, Margin, Modifiers, Pos2, RichText, Sense, Stroke, TextEdit,
    TextureHandle, TextureOptions, Vec2,
};
use egui_extras::{Size, StripBuilder};

use crate::models::lua_vault::{LuaVariant, LuaVault};
use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::parsers::steam_link::SteamLinkParser;
use crate::services::api_client::{
    DRMFixFixItem, DRMFixGameListing, DRMFixTag, DlcInfo, HubcapService, LuaToolsApiClient,
    SteamFeaturedItem, SteamSearchItem,
};
use crate::services::auth_service::AuthService;
use crate::services::caches::{
    CoverCache, GameDetails, HardwareAppIdService, SteamAppInfoCache, SteamAppListCache,
};
use crate::services::cef_bridge::CefBridgeService;
use crate::services::depot_downloader::DepotDownloaderService;
use crate::services::depot_info::{AppDepotInfo, ContentDepot, SteamDepotInfo};
use crate::services::donate_keys::DonateKeysService;
use crate::services::external_tools::ExternalToolsManager;
use crate::services::fixes_service::FixesService;
use crate::services::http_server::{HttpServerService, RpcUiEvent};
use crate::services::lua_installer::LuaInstaller;
use crate::services::millennium_service::MillenniumService;
use crate::services::play_unowned_service::PlayUnownedService;
use crate::services::protocol_service::ProtocolService;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
use crate::services::steam_library_service::SteamLibraryService;
use crate::services::steam_service::SteamService;
use crate::services::wired_service::WiredService;
use crate::services::telemetry_service::TelemetryService;
use crate::services::update_service::UpdateService;
use crate::ui::state::{
    AcquireTab, ActiveView, AddByAppIdModal, AppMessage, DepotDetailModal, DepotPickRow,
    DepotsSubTab, GameDetailModal, GuideTopic, LaunchOptionsModal, LibraryRow, QaCaptureState,
    SettingsSubTab, TextPromptModal, ToastKind, ToastNotification, UpdateBannerState,
};
use crate::ui::theme::{
    apply_papercut_monotone_theme, display_font, draw_rem_medallion, nav_rail_button,
    primary_button, semibold_font, Palette,
};
use crate::VERSION;

pub struct LuaToolsApp {
    // Core Services
    pub settings_service: SettingsService,
    pub auth_service: AuthService,
    pub steam_service: SteamService,
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
    pub lua_vault: LuaVault,
    pub lua_installer: Option<LuaInstaller>,
    pub play_unowned_service: PlayUnownedService,
    pub steam_library_service: SteamLibraryService,
    pub wired_service: WiredService,
    pub cef_bridge: CefBridgeService,
    pub http_server: Option<HttpServerService>,
    pub tools_manager: ExternalToolsManager,
    pub telemetry_service: TelemetryService,

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
    pub cover_in_flight: HashSet<u32>,
    pub recent_installed: Vec<(u32, PathBuf)>,

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
    pub update_banner: UpdateBannerState,
    pub guide_topic: GuideTopic,

    // Modals & QA
    pub add_appid_modal: AddByAppIdModal,
    pub game_detail_modal: GameDetailModal,
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

        let slssteam_service_clone = slssteam_service.clone();
        let steam_installation = steam_service.installation.clone();
        let mut app = Self {
            settings_service,
            auth_service,
            steam_service,
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
            lua_vault,
            lua_installer: None,
            play_unowned_service: PlayUnownedService::new(
                steam_installation.clone().unwrap_or_default(),
                slssteam_service_clone.clone(),
            ),
            steam_library_service: SteamLibraryService::new(
                steam_installation.clone().unwrap_or_default(),
                slssteam_service_clone.clone(),
            ),
            wired_service: WiredService::new(),
            cef_bridge,
            http_server: None,
            tools_manager,
            cef_bridge,
            http_server: None,
            tools_manager,
            telemetry_service,
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
            cover_in_flight: HashSet::new(),
            recent_installed: Vec::new(),
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
            settings_millennium_status: "Click Probe Bridge to check Millennium RPC (:38495)"
                .into(),
            settings_update_status: "Checked automatically once on startup".into(),
            settings_telemetry_status: String::new(),
            update_banner: UpdateBannerState::Hidden,
            guide_topic: GuideTopic::QuickStart,
            add_appid_modal: AddByAppIdModal::default(),
            game_detail_modal: GameDetailModal::default(),
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
            app.probe_bridge_async(&cc.egui_ctx);
            app.fetch_featured_async(&cc.egui_ctx);
            app.fetch_fix_listings_async(&cc.egui_ctx);
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

        app
    }

    /// Headless constructor used by offline visual & layout QA tests (uses `temp_dir` paths only).
    pub fn new_headless_demo(ctx: &egui::Context, scratch_dir: &Path) -> Self {
        apply_papercut_monotone_theme(ctx);
        let _ = fs::create_dir_all(scratch_dir);
        let settings_service = SettingsService::new(Some(scratch_dir.join("settings.json")));
        let auth_service = AuthService::new(Some(scratch_dir.join("auth.json")));
        let steam_service = SteamService::new();
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
        let (msg_tx, msg_rx) = mpsc::channel();
        let (rpc_tx, rpc_rx) = mpsc::channel();

        let mut app = Self {
            settings_service,
            auth_service,
            steam_service,
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
            lua_vault,
            lua_installer: None,
            play_unowned_service: PlayUnownedService::new(
                steam_installation.clone().unwrap_or_default(),
                slssteam_service_clone.clone(),
            ),
            steam_library_service: SteamLibraryService::new(
                steam_installation.clone().unwrap_or_default(),
                slssteam_service_clone.clone(),
            ),
            cef_bridge,
            http_server: None,
            tools_manager,
            telemetry_service,
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
            cover_in_flight: HashSet::new(),
            recent_installed: vec![
                (1245620, PathBuf::from("1245620.lua")),
                (1091500, PathBuf::from("1091500.lua")),
            ],
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
            settings_millennium_status: "Click Probe Millennium to check local RPC (:38495)".into(),
            settings_update_status: format!("Up to date (v{}) · Checked once on startup", VERSION),
            settings_telemetry_status: String::new(),
            update_banner: UpdateBannerState::Hidden,
            guide_topic: GuideTopic::QuickStart,
            add_appid_modal: AddByAppIdModal::default(),
            game_detail_modal: GameDetailModal::default(),
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
        self.update_banner = UpdateBannerState::Available {
            badge: "⬆ Update v2.0".into(),
            tooltip: "New commits available on GitHub".into(),
        };
    }

    pub fn detect_steam_and_sync(&mut self) {
        let ov = self.settings_service.settings.steam_path_override.clone();
        if let Some(inst) = self.steam_service.detect(Some(&ov)) {
            self.lua_vault.steam_installation = Some(inst.clone());
            self.depot_downloader
                .with_steam(inst.clone(), self.slssteam_service.clone());
            let installer = LuaInstaller::new(inst.clone(), self.lua_vault.clone());
            self.lua_installer = Some(installer);

            let sls_for_play = self.slssteam_service.clone();
            self.play_unowned_service = PlayUnownedService::new(inst.clone(), sls_for_play);

            self.steam_library_service =
                SteamLibraryService::new(inst.clone(), self.slssteam_service.clone());

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

            self.refresh_all_local_state();
            self.status_text = format!(
                "Steam connected at {} ({} library folder(s))",
                inst.root.display(),
                inst.libraries.len()
            );
        } else {
            self.lua_installer = None;
            self.depot_downloader.steam_installation = None;
            self.play_unowned_service = PlayUnownedService::new(
                SteamService::new().installation.unwrap_or_default(),
                self.slssteam_service.clone(),
            );
            self.steam_library_service =
                SteamLibraryService::new(SteamService::new().installation.unwrap_or_default(), self.slssteam_service.clone());
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
        self.telemetry_service.update_runtime_context(
            self.steam_service.is_available(),
            self.steam_service.libraries().len(),
            self.slssteam_service.config.exists(),
        );
        if let Some(installer) = &self.lua_installer {
            self.recent_installed = installer.enumerate_installed();
        }
        self.refresh_library_rows();
    }

    pub fn refresh_library_rows(&mut self) {
        let Some(inst) = &self.steam_service.installation else {
            self.library_rows.clear();
            return;
        };
        self.applist_cache.ensure_loaded();

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

        let mut rows = Vec::with_capacity(all_ids.len());
        for aid in all_ids {
            let is_inst = installed_names.contains_key(&aid);
            let has_lua = lua_ids.contains(&aid);
            let fake_id = self.slssteam_service.get_fake_app_id(aid);
            let has_token = self.slssteam_service.get_token(aid).is_some();
            let name = installed_names
                .get(&aid)
                .cloned()
                .or_else(|| self.applist_cache.get_name(aid))
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
    }

    pub fn resolve_game_title(&self, app_id: u32) -> String {
        self.steam_service
            .get_game_name(app_id)
            .or_else(|| self.applist_cache.get_name(app_id))
            .unwrap_or_else(|| format!("App {}", app_id))
    }

    /// Shows a non-blocking top-right toast notification (never placed at the bottom).
    pub fn notify(&mut self, msg: impl Into<String>, kind: ToastKind) {
        let text = msg.into();
        if kind == ToastKind::Warning {
            self.telemetry_service
                .record("warn", "diagnostic", "warning_toast", &text);
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

    pub fn ensure_cover_requested(
        &mut self,
        app_id: u32,
        custom_url: Option<String>,
        ctx: &egui::Context,
    ) {
        if self.cover_textures.contains_key(&app_id) || !self.cover_in_flight.insert(app_id) {
            return;
        }
        let cc = self.cover_cache.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        thread::spawn(move || {
            if let Some(path) = cc.ensure(app_id, custom_url.as_deref()) {
                if let Ok(bytes) = fs::read(&path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let _ = tx.send(AppMessage::CoverLoaded {
                            app_id,
                            width: w as usize,
                            height: h as usize,
                            rgba: rgba.into_raw(),
                        });
                        ctx_c.request_repaint();
                    }
                }
            }
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
        self.ensure_cover_requested(app_id, None, ctx);

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
            self.acquire_status = "Steam installation not detected.".into();
            return;
        }
        if source != "hubcap" && !self.auth_service.is_signed_in() {
            self.acquire_status =
                "Sign in with Discord in Settings › Account first (or configure a Hubcap key)."
                    .into();
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
            self.acquire_status = "Sign in with Discord first.".into();
            return;
        }
        self.acquire_busy = true;
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
        self.active_view = ActiveView::Depots;
        self.depots_app_id = Some(app_id);
        self.depots_editing_live = false;
        self.depots_selected_variant_hash = None;
        self.depots_info = None;

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

    pub fn reload_depots_local(&mut self, app_id: u32) {
        let live_text = self.lua_vault.read_live_text(app_id);
        self.depots_lua =
            (!live_text.trim().is_empty()).then(|| LuaFileParser::parse_text(&live_text, app_id));
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
            aid,
        );

        for entry in lua.entries {
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
                    let _ = tx.send(AppMessage::ToolFinished {
                        tool_key: k_owned,
                        ok: true,
                        msg: format!("Ready at {}", exe.display()),
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

    pub fn trigger_donate_keys(&mut self, ctx: &egui::Context) {
        let Some(inst) = &self.steam_service.installation else {
            self.status_text = "Steam installation not detected.".into();
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
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();
        self.status_text = "Sending RestartSteam RPC to Millennium…".into();
        thread::spawn(move || {
            match m.restart_steam() {
                Ok(()) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: true,
                        msg: "Restarting Steam…".into(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppMessage::SteamRestarted { ok: false, msg: e });
                }
            }
            ctx_c.request_repaint();
        });
    }

    pub fn handle_protocol_url(&mut self, url: &str, ctx: &egui::Context) {
        let Some(act) = ProtocolService::parse(url) else {
            return;
        };
        match act.kind.as_str() {
            "game" | "install" => {
                self.open_app_in_acquire(act.app_id, ctx);
                if let Some(src) = act.source {
                    self.acquire_selected_source = Some(src);
                }
            }
            "manage" => self.open_game_in_depots(act.app_id, ctx),
            "fix" => self.open_app_in_fixes(act.app_id, ctx),
            _ => {}
        }
    }

    pub fn install_dropped_paths(&mut self, paths: &[PathBuf]) {
        let Some(installer) = self.lua_installer.clone() else {
            self.status_text = "Steam installation not detected yet.".into();
            return;
        };
        let mut ok_count = 0usize;
        let mut last_err = None;
        for p in paths {
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
        while let Ok(ev) = self.rpc_rx.try_recv() {
            match ev {
                RpcUiEvent::AddApp(aid) => self.open_app_in_acquire(aid, ctx),
                RpcUiEvent::OpenFix(aid) => self.open_app_in_fixes(aid, ctx),
                RpcUiEvent::OpenSettings => self.active_view = ActiveView::Settings,
                RpcUiEvent::RemovedApp(_) => self.refresh_all_local_state(),
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
                    let img = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    let tex =
                        ctx.load_texture(format!("cover_{}", app_id), img, TextureOptions::LINEAR);
                    self.cover_textures.insert(app_id, tex);
                }
                AppMessage::FeaturedLoaded { top, new_releases } => {
                    self.featured_top = top;
                    self.featured_new = new_releases;
                }
                AppMessage::SearchFinished { items, error } => {
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
                    if self.acquire_selected_app == Some(app_id) {
                        if let Some(ref d) = details {
                            if !d.header_image.is_empty() {
                                self.ensure_cover_requested(
                                    app_id,
                                    Some(d.header_image.clone()),
                                    ctx,
                                );
                            }
                        }
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
                    self.acquire_progress = pct;
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
                    if ok {
                        if let (Some(dl), Some(installer)) =
                            (downloaded_path, self.lua_installer.clone())
                        {
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
                                self.acquire_status = msg.clone();
                                self.notify(msg, ToastKind::Success);
                                self.refresh_all_local_state();
                            } else {
                                let err = res.error.unwrap_or_else(|| "Install failed".into());
                                self.acquire_status = err.clone();
                                self.notify(err, ToastKind::Warning);
                            }
                        }
                    } else {
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
                        self.depots_dl_pct = pct;
                    }
                    let shown_pct = if pct >= 0.0 { pct } else { self.depots_dl_pct };
                    self.depots_dl_status =
                        format!("{} · {:.1}% — {}", depot_label, shown_pct, msg);
                }
                AppMessage::DepotDownloadFinished { results } => {
                    self.depots_downloading = false;
                    let ok = results.iter().filter(|r| r.ok).count();
                    let total = results.len();
                    let first_err = results.iter().find_map(|r| r.error.as_deref());
                    if let Some(err) = first_err {
                        let summary = format!("Downloaded {}/{} depot(s) — {}", ok, total, err);
                        self.depots_dl_status = summary.clone();
                        self.notify(summary, ToastKind::Warning);
                    } else {
                        let summary =
                            format!("Completed · {} of {} depot(s) downloaded", ok, total);
                        self.depots_dl_status = summary.clone();
                        self.notify(summary, ToastKind::Success);
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
                    self.fixes_progress = pct;
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
                    if ok {
                        if let (Some(dl), Some(installer)) =
                            (downloaded_manifest, self.lua_installer.clone())
                        {
                            let _ = installer.install_any(
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
                        }
                    }
                    self.notify(
                        msg,
                        if ok {
                            ToastKind::Success
                        } else {
                            ToastKind::Warning
                        },
                    );
                }
                AppMessage::ToolProgress { tool_key, pct, msg } => {
                    self.tool_progress.insert(tool_key, pct);
                    self.status_text = msg;
                }
                AppMessage::ToolFinished { tool_key, ok, msg } => {
                    self.tool_busy.insert(tool_key, false);
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
                AppMessage::HubcapChecked { ok: _, msg } => {
                    self.settings_hubcap_status = msg;
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
                    match self.tools_manager.launch_steamautopatch(aid, &dir) {
                        Ok(m) | Err(m) => self.status_text = m,
                    }
                }
                AppMessage::FilePickedForStubRemover(exe) => {
                    match self.tools_manager.launch_stub_remover(&exe) {
                        Ok(m) | Err(m) => self.status_text = m,
                    }
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
                    self.update_banner = UpdateBannerState::Updating { pct, step: msg };
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
            }
        }
    }

    fn handle_shortcuts_and_drops(&mut self, ctx: &egui::Context) {
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
                self.active_view = ActiveView::Settings;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Q)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
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
                .collect()
        });
        if !dropped.is_empty() {
            self.install_dropped_paths(&dropped);
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
        let p = Path::new(&raw);
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
        if self.active_view == ActiveView::Depots && self.depots_app_id.is_none() {
            if let Some((first_aid, _)) = self.recent_installed.first().cloned() {
                self.open_game_in_depots(first_aid, ctx);
            } else if let Some(first_row) = self.library_rows.first() {
                self.open_game_in_depots(first_row.app_id, ctx);
            }
        }

        // 1. Top Bar — Strict 3-Column StripBuilder (mathematically zero overlap at any width)
        //    Houses Brand (left), Omnibox (center), and Steam status + corner Git Update button (top-right).
        let mut run_omnibox = false;
        let mut click_rescan = false;
        let mut click_start_update = false;
        let mut click_relaunch_exe: Option<PathBuf> = None;
        let right_col_w = if matches!(self.update_banner, UpdateBannerState::Hidden) {
            156.0
        } else {
            288.0
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
                    .size(Size::exact(196.0))
                    .size(Size::remainder().at_least(210.0))
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

                        // Center: Single Clean Bounded Omnibox + Search Button
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
                                    .hint_text("Search Store, AppID, or paste Steam URL…  (Ctrl+K)"),
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

                        // Right: Steam Status Pill + Corner Git Update Button (never placed at the bottom!)
                        strip.cell(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;

                                let online = self.steam_service.is_available();
                                let (lbl, bg, fg) = if online {
                                    ("● Steam Ready", Palette::GO_BG, Palette::GO_TEXT)
                                } else {
                                    ("○ No Steam", Palette::WARN_BG, Palette::WARN_TEXT)
                                };
                                let g = ui.painter().layout_no_wrap(
                                    lbl.to_string(),
                                    semibold_font(11.5),
                                    fg,
                                );
                                let sz = Vec2::new(g.size().x + 22.0, 28.0);
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
                                    ui.painter().galley(
                                        Pos2::new(
                                            rect.center().x - g.size().x * 0.5,
                                            rect.center().y - g.size().y * 0.5,
                                        ),
                                        g,
                                        fg,
                                    );
                                }
                                let tip = match &self.steam_service.installation {
                                    Some(i) => format!(
                                        "Steam root: {}\nLibraries: {}\nClick to rescan",
                                        i.root.display(),
                                        i.libraries.len()
                                    ),
                                    None => "Steam not detected. Click to rescan or configure path in Settings.".into(),
                                };
                                if pill_resp.on_hover_text(tip).clicked() {
                                    click_rescan = true;
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
        if click_start_update {
            self.trigger_git_self_update(ctx);
        }
        if let Some(exe) = click_relaunch_exe {
            match UpdateService::relaunch_binary(&exe) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
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
                        .clicked()
                        {
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
                            ui.label(RichText::new(acct).size(11.5).color(
                                if self.auth_service.is_signed_in() {
                                    Palette::BLUE_LIGHT
                                } else {
                                    Palette::TEXT_MUTED
                                },
                            ));
                        });
                    });

                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(
                        RichText::new("WORKSPACES")
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
                let items = [
                    ("Home", None, ActiveView::Overview),
                    ("Discover", None, ActiveView::Acquire),
                    ("Library", Some(lua_n), ActiveView::Library),
                    ("Depots & Vault", None, ActiveView::Depots),
                    ("Game Fixes", None, ActiveView::Fixes),
                    ("How to Use", None, ActiveView::Guide),
                ];

                for (lbl, count, view) in items {
                    if nav_rail_button(ui, lbl, count, self.active_view == view, 204.0).clicked() {
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
        egui::Area::new(egui::Id::new("top_toast_area"))
            .anchor(egui::Align2::CENTER_TOP, Vec2::new(96.0, 64.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
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
                                .wrap_mode(egui::TextWrapMode::Extend),
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
        self.handle_shortcuts_and_drops(ctx);
        self.render_shell(ctx);
        self.drive_qa_screenshots(ctx);
    }
}
