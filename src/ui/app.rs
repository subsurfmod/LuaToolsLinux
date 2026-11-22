//! Main `LuaToolsApp` state coordinator & architectural window shell (`eframe::App`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use eframe::egui::{
    self, Align, ColorImage, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText,
    Stroke, TextEdit, TextureHandle, TextureOptions,
};

use crate::models::lua_vault::{LuaVariant, LuaVault};
use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::parsers::steam_link::SteamLinkParser;
use crate::services::api_client::{
    DenuvoFixItem, DenuvoGameListing, DenuvoTag, DlcInfo, HubcapService, LuaToolsApiClient,
    SteamFeaturedItem, SteamSearchItem,
};
use crate::services::auth_service::AuthService;
use crate::services::caches::{
    CoverCache, GameDetails, HardwareAppIdService, SteamAppInfoCache, SteamAppListCache,
};
use crate::services::cef_injector::CefInjectorService;
use crate::services::depot_downloader::DepotDownloaderService;
use crate::services::depot_info::{AppDepotInfo, ContentDepot, SteamDepotInfo};
use crate::services::donate_keys::DonateKeysService;
use crate::services::external_tools::ExternalToolsManager;
use crate::services::fixes_service::FixesService;
use crate::services::http_server::{HttpServerService, RpcUiEvent};
use crate::services::lua_installer::LuaInstaller;
use crate::services::millennium_service::MillenniumService;
use crate::services::protocol_service::ProtocolService;
use crate::services::settings_service::SettingsService;
use crate::services::slssteam_service::SlssteamService;
use crate::services::steam_service::SteamService;
use crate::services::update_service::UpdateService;
use crate::ui::state::{
    AcquireTab, ActiveView, AddByAppIdModal, AppMessage, DepotDetailModal, DepotPickRow,
    DepotsSubTab, GameDetailModal, LaunchOptionsModal, LibraryRow, TextPromptModal,
    UpdateBannerState,
};
use crate::ui::theme::{
    apply_sharp_monotone_theme, ghost_button, nav_rail_button, primary_button, status_badge,
    BadgeTone, Palette,
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
    pub cef_injector: CefInjectorService,
    pub http_server: Option<HttpServerService>,
    pub tools_manager: ExternalToolsManager,

    // Async Channels
    pub msg_tx: Sender<AppMessage>,
    pub msg_rx: Receiver<AppMessage>,
    pub rpc_tx: Sender<RpcUiEvent>,
    pub rpc_rx: Receiver<RpcUiEvent>,

    // Shell State
    pub active_view: ActiveView,
    pub command_input: String,
    pub focus_command_next: bool,
    pub status_text: String,
    pub millennium_connected: bool,
    pub cover_textures: HashMap<u32, TextureHandle>,
    pub cover_in_flight: HashSet<u32>,
    pub dropzone_url_input: String,
    pub recent_installed: Vec<(u32, PathBuf)>,

    // Acquire View State
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
    pub fixes_listings: Vec<DenuvoGameListing>,
    pub fixes_tags: Vec<DenuvoTag>,
    pub fixes_selected_tag: Option<String>,
    pub fixes_search: String,
    pub fixes_my_games_only: bool,
    pub fixes_current_app: Option<u32>,
    pub fixes_items: Vec<DenuvoFixItem>,
    pub fixes_selected_row: Option<usize>,
    pub fixes_busy: bool,
    pub fixes_progress: f32,

    // Toolchain View State
    pub tool_busy: HashMap<String, bool>,
    pub tool_progress: HashMap<String, f32>,

    // Settings & Self-Updater State
    pub settings_show_hubcap: bool,
    pub settings_hubcap_status: String,
    pub settings_usage_text: String,
    pub settings_millennium_status: String,
    pub settings_update_status: String,
    pub update_banner: UpdateBannerState,

    // Modals
    pub add_appid_modal: AddByAppIdModal,
    pub game_detail_modal: GameDetailModal,
    pub launch_options_modal: LaunchOptionsModal,
    pub depot_detail_modal: DepotDetailModal,
    pub local_ctx_initialized: bool,
    pub text_prompt_modal: TextPromptModal,
    pub about_modal_open: bool,
}

impl LuaToolsApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_url: Option<String>) -> Self {
        apply_sharp_monotone_theme(&cc.egui_ctx);

        let settings_service = SettingsService::default();
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
        let cef_injector =
            CefInjectorService::new(None, settings_service.settings.http_server_port);
        let tools_manager = ExternalToolsManager::default();

        let (msg_tx, msg_rx) = mpsc::channel();
        let (rpc_tx, rpc_rx) = mpsc::channel();

        let default_dest = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("LuaTools Depots")
            .to_string_lossy()
            .to_string();

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
            cef_injector,
            http_server: None,
            tools_manager,
            msg_tx,
            msg_rx,
            rpc_tx,
            rpc_rx,
            active_view: ActiveView::Overview,
            command_input: String::new(),
            focus_command_next: false,
            status_text: "Initializing LuaTools Linux v2.0...".into(),
            millennium_connected: false,
            cover_textures: HashMap::new(),
            cover_in_flight: HashSet::new(),
            dropzone_url_input: String::new(),
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
            tool_busy: HashMap::new(),
            tool_progress: HashMap::new(),
            settings_show_hubcap: false,
            settings_hubcap_status: String::new(),
            settings_usage_text: String::new(),
            settings_millennium_status: "Click PROBE MILLENNIUM to verify RPC bridge on :38495"
                .into(),
            settings_update_status: "Idle (runs once in background on startup)".into(),
            update_banner: UpdateBannerState::Hidden,
            add_appid_modal: AddByAppIdModal::default(),
            game_detail_modal: GameDetailModal::default(),
            launch_options_modal: LaunchOptionsModal::default(),
            depot_detail_modal: DepotDetailModal::default(),
            local_ctx_initialized: false,
            text_prompt_modal: TextPromptModal::default(),
            about_modal_open: false,
        };

        app.detect_steam_and_sync();
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

        app
    }

    pub fn detect_steam_and_sync(&mut self) {
        let ov = self.settings_service.settings.steam_path_override.clone();
        if let Some(inst) = self.steam_service.detect(Some(&ov)) {
            self.lua_vault.steam_installation = Some(inst.clone());
            let installer = LuaInstaller::new(inst.clone(), self.lua_vault.clone());
            self.lua_installer = Some(installer);

            self.cef_injector.steam_root = Some(inst.root.clone());
            if self.settings_service.settings.cef_injector_enabled {
                self.cef_injector.start();
            }

            if self.settings_service.settings.http_server_enabled && self.http_server.is_none() {
                self.http_server = HttpServerService::start(
                    self.settings_service.settings.http_server_port,
                    inst.stplug_in_path.clone(),
                    self.rpc_tx.clone(),
                    None,
                );
            }

            if self.settings_service.settings.donate_keys {
                let dk = DonateKeysService::new(inst.root.clone(), None);
                thread::spawn(move || {
                    let _ = dk.donate_new_keys();
                });
            }

            self.refresh_all_local_state();
            self.status_text = format!(
                "Ready — Steam detected at {} ({} library folder(s))",
                inst.root.display(),
                inst.libraries.len()
            );
        } else {
            self.lua_installer = None;
            self.recent_installed.clear();
            self.library_rows.clear();
            self.status_text =
                "Steam not detected at standard paths. Set an override path in 07 // SYSTEM if needed."
                    .into();
        }
    }

    pub fn refresh_all_local_state(&mut self) {
        self.slssteam_service.reload();
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
                (true, true) => "INSTALLED + LUA",
                (false, true) => "LUA ONLY",
                _ => "INSTALLED",
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
            let (games, tags) = api.get_denuvo_listings();
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
                parts.push(format!("DAILY QUOTA: {}/{}", u, lim));
            }
            if let Some(sup) = api.get_supporter_status() {
                parts.push(format!("SUPPORTER: {}", if sup { "YES" } else { "NO" }));
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
        self.acquire_status = format!("Searching Steam Store for '{}'...", q);
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
        self.acquire_status = format!(
            "Fetching details & manifest sources for AppID {}...",
            app_id
        );
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
                "Sign in with Discord in 07 // SYSTEM first (or use a Hubcap key).".into();
            return;
        }
        self.acquire_busy = true;
        self.acquire_progress = 0.0;
        self.acquire_status = format!("Downloading manifest package from {}...", source);

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
            &self.slssteam_service,
            aid,
        );

        for entry in lua.entries {
            if !entry.has_key() {
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
            } else if !has_key {
                Some("Missing decryption key".into())
            } else if manifest_id.is_none() {
                Some("No manifest GID".into())
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
            let fixes = api.get_denuvo_fixes(app_id);
            let _ = tx.send(AppMessage::FixesLoaded { app_id, fixes });
            ctx_c.request_repaint();
        });
    }

    pub fn apply_fix_manifest_async(
        &mut self,
        app_id: u32,
        fix: DenuvoFixItem,
        ctx: &egui::Context,
    ) {
        if !self.auth_service.is_signed_in() {
            self.status_text = "Sign in with Discord in 07 // SYSTEM first.".into();
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
            match api.download_denuvo_slot(&fix.id, "manifest", &tmp, &fb, move |p, m| {
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
        fix: DenuvoFixItem,
        install_dir: PathBuf,
        ctx: &egui::Context,
    ) {
        if !self.auth_service.is_signed_in() {
            self.status_text = "Sign in with Discord in 07 // SYSTEM first.".into();
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
                FixesService::apply_denuvo_fix(&api, app_id, &fix, &install_dir, move |p, m| {
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
        self.settings_update_status = "Checking Git remote for updates...".into();
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
            step: "Starting Git pull & update...".into(),
        };
        self.status_text = "Updating from Git...".into();
        self.settings_update_status = "Pulling from Git & rebuilding release binary...".into();

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
        self.status_text = "Sending RestartSteam RPC to Millennium...".into();
        thread::spawn(move || {
            match m.restart_steam() {
                Ok(()) => {
                    let _ = tx.send(AppMessage::SteamRestarted {
                        ok: true,
                        msg: "Restarting Steam...".into(),
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
            "game" | "install" => self.open_app_in_acquire(act.app_id, ctx),
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
            self.status_text = format!("Installed {} dropped package(s) into Steam.", ok_count);
        } else if let Some(e) = last_err {
            self.status_text = format!("Drop install error: {}", e);
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
                            "CONNECTED :: Millennium v{}  ·  Steam={}",
                            version.unwrap_or_else(|| "unknown".into()),
                            steam_path.unwrap_or_else(|| "unknown".into())
                        )
                    } else {
                        "OFFLINE :: Millennium RPC not reachable on 127.0.0.1:38495".into()
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
                                self.acquire_status = format!(
                                    "Installed AppID {} ({} manifest file(s))",
                                    app_id, res.manifest_count
                                );
                                self.status_text = self.acquire_status.clone();
                                self.refresh_all_local_state();
                            } else {
                                self.acquire_status =
                                    res.error.unwrap_or_else(|| "Install failed".into());
                            }
                        }
                    } else {
                        self.acquire_status = msg;
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
                    if pct > 0.0 {
                        self.depots_dl_pct = pct;
                    }
                    self.depots_dl_status = format!("{} :: {:.1}% — {}", depot_label, pct, msg);
                }
                AppMessage::DepotDownloadFinished { results } => {
                    self.depots_downloading = false;
                    let ok = results.iter().filter(|r| r.ok).count();
                    let total = results.len();
                    self.depots_dl_status =
                        format!("COMPLETED :: {} / {} depot(s) downloaded", ok, total);
                    self.status_text = self.depots_dl_status.clone();
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
                    self.status_text = msg;
                }
                AppMessage::ToolProgress { tool_key, pct, msg } => {
                    self.tool_progress.insert(tool_key, pct);
                    self.status_text = msg;
                }
                AppMessage::ToolFinished {
                    tool_key,
                    ok: _,
                    msg,
                } => {
                    self.tool_busy.insert(tool_key, false);
                    self.status_text = msg;
                }
                AppMessage::AuthCompleted { ok, msg } => {
                    self.status_text = msg;
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
                    self.status_text = if count > 0 {
                        format!("Submitted {} new decryption key(s) from config.vdf.", count)
                    } else {
                        "No new decryption keys to submit.".into()
                    };
                }
                AppMessage::SteamRestarted { ok: _, msg } => {
                    self.status_text = msg;
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
                    match self.tools_manager.launch_steamautocrack(aid, &dir) {
                        Ok(m) | Err(m) => self.status_text = m,
                    }
                }
                AppMessage::FilePickedForSteamless(exe) => {
                    match self.tools_manager.launch_steamless(&exe) {
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
                            (Some(v), _) if v != VERSION => format!("⬆ UPDATE v{}", v),
                            (_, b) if b > 0 => format!("⬆ UPDATE (+{})", b),
                            _ => "⬆ UPDATE".into(),
                        };
                        let mut tip = chk.summary.clone();
                        if let Some(c) = &chk.latest_commit {
                            tip.push_str(&format!("\nLatest: {}", c));
                        }
                        tip.push_str("\nClick to pull latest files from Git and update binary.");
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
                            "Updated to v{} ({}) — click ↻ RESTART in the bottom-right corner to relaunch.",
                            res.new_version, res.commit_info
                        );
                        self.status_text = summary.clone();
                        self.settings_update_status = summary.clone();
                        self.update_banner = UpdateBannerState::ReadyToRestart {
                            new_version: res.new_version,
                            exe_path: res.updated_exe,
                            summary,
                        };
                    }
                    Err(err) => {
                        self.status_text = format!("Update failed: {}", err);
                        self.settings_update_status = format!("ERROR :: {}", err);
                        self.update_banner = UpdateBannerState::Available {
                            badge: "⬆ RETRY UPDATE".into(),
                            tooltip: format!(
                                "Last attempt failed: {}\nClick to retry Git pull & update.",
                                err
                            ),
                        };
                    }
                },
                AppMessage::FolderPickedForUpdateRepo(dir) => {
                    self.settings_service.settings.update_repo_dir =
                        dir.to_string_lossy().to_string();
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
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num6)) {
                self.active_view = ActiveView::Toolchain;
            }
            if i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Num7)) {
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
            self.status_text = format!("Opened AppID {} in Acquire Inspector", aid);
            return;
        }

        self.acquire_search_query = raw.clone();
        self.active_view = ActiveView::Acquire;
        self.execute_store_search(&raw, ctx);
    }
}

impl eframe::App for LuaToolsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_messages(ctx);
        self.handle_shortcuts_and_drops(ctx);

        // Auto-populate Depots view with most recent Lua game when visiting Depots
        if self.active_view == ActiveView::Depots && self.depots_app_id.is_none() {
            if let Some((first_aid, _)) = self.recent_installed.first().cloned() {
                self.open_game_in_depots(first_aid, ctx);
            } else if let Some(first_row) = self.library_rows.first() {
                self.open_game_in_depots(first_row.app_id, ctx);
            }
        }

        // 1. Top Command Bar
        egui::TopBottomPanel::top("top_command_bar")
            .exact_height(48.0)
            .frame(
                Frame::new()
                    .fill(Palette::BG_VOID)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                    .inner_margin(Margin::symmetric(14, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    Frame::new()
                        .fill(Palette::BLUE_PRIMARY)
                        .stroke(Stroke::new(1.0_f32, Palette::BLUE_LIGHT))
                        .inner_margin(Margin::symmetric(6, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("LT")
                                    .monospace()
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );
                        });

                    ui.label(
                        RichText::new("LUATOOLS // LINUX")
                            .monospace()
                            .strong()
                            .size(12.5)
                            .color(Palette::TEXT_PRIMARY),
                    );
                    ui.label(
                        RichText::new(format!("v{}", VERSION))
                            .monospace()
                            .size(10.5)
                            .color(Palette::TEXT_MUTED),
                    );
                    ui.add_space(6.0);

                    let cmd_w = (ui.available_width() - 395.0).max(180.0);
                    let cmd_resp = ui.add(
                        TextEdit::singleline(&mut self.command_input)
                            .hint_text(
                                "Quick Command [Ctrl+K] — Enter AppID, paste Steam / SteamDB URL, or search Store...",
                            )
                            .desired_width(cmd_w),
                    );
                    if self.focus_command_next {
                        cmd_resp.request_focus();
                        self.focus_command_next = false;
                    }
                    let enter = cmd_resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if enter || primary_button(ui, "EXECUTE").clicked() {
                        self.execute_quick_command(ctx);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ghost_button(ui, "ABOUT").clicked() {
                            self.about_modal_open = true;
                        }
                        if ghost_button(ui, "RESTART STEAM").clicked() {
                            self.trigger_restart_steam(ctx);
                        }
                        if ghost_button(ui, "RESCAN").clicked() {
                            self.detect_steam_and_sync();
                        }
                        if self.steam_service.is_available() {
                            status_badge(ui, "STEAM ONLINE", BadgeTone::Blue);
                        } else {
                            status_badge(ui, "NO STEAM", BadgeTone::Warn);
                        }
                    });
                });
            });

        // 2. Bottom Status Strip (with compact bottom-right corner Git Update button)
        let mut click_start_update = false;
        let mut click_relaunch_exe: Option<PathBuf> = None;

        egui::TopBottomPanel::bottom("bottom_status_strip")
            .exact_height(28.0)
            .frame(
                Frame::new()
                    .fill(Palette::BG_VOID)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                    .inner_margin(Margin::symmetric(14, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        RichText::new(&self.status_text)
                            .monospace()
                            .size(10.5)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        match &self.update_banner {
                            UpdateBannerState::Hidden => {}
                            UpdateBannerState::Available { badge, tooltip } => {
                                let btn = egui::Button::new(
                                    RichText::new(badge)
                                        .monospace()
                                        .strong()
                                        .size(10.5)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(Palette::BLUE_PRIMARY)
                                .stroke(Stroke::new(1.0_f32, Palette::BLUE_LIGHT))
                                .corner_radius(egui::CornerRadius::ZERO)
                                .min_size(egui::Vec2::new(96.0, 20.0));
                                let resp = ui.add(btn).on_hover_text(tooltip);
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if resp.clicked() {
                                    click_start_update = true;
                                }
                                ui.add_space(8.0);
                            }
                            UpdateBannerState::Updating { pct, step } => {
                                let lbl = format!("⟳ UPDATING {:>2.0}%", pct * 100.0);
                                let btn = egui::Button::new(
                                    RichText::new(lbl)
                                        .monospace()
                                        .strong()
                                        .size(10.5)
                                        .color(Palette::BLUE_LIGHT),
                                )
                                .fill(Palette::BLUE_SUBTLE)
                                .stroke(Stroke::new(1.0_f32, Palette::BLUE_BORDER))
                                .corner_radius(egui::CornerRadius::ZERO)
                                .min_size(egui::Vec2::new(108.0, 20.0));
                                ui.add_enabled(false, btn).on_disabled_hover_text(step);
                                ui.add_space(8.0);
                            }
                            UpdateBannerState::ReadyToRestart {
                                new_version,
                                exe_path,
                                summary,
                            } => {
                                let btn = egui::Button::new(
                                    RichText::new(format!("↻ RESTART (v{})", new_version))
                                        .monospace()
                                        .strong()
                                        .size(10.5)
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(Palette::BLUE_HOVER)
                                .stroke(Stroke::new(1.0_f32, Palette::BLUE_LIGHT))
                                .corner_radius(egui::CornerRadius::ZERO)
                                .min_size(egui::Vec2::new(112.0, 20.0));
                                let resp = ui.add(btn).on_hover_text(summary);
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if resp.clicked() {
                                    click_relaunch_exe = Some(exe_path.clone());
                                }
                                ui.add_space(8.0);
                            }
                        }

                        let path_txt = match &self.steam_service.installation {
                            Some(i) => format!("{}  [{} LIB]", i.root.display(), i.libraries.len()),
                            None => "STEAM :: NOT DETECTED".into(),
                        };
                        ui.label(
                            RichText::new(path_txt)
                                .monospace()
                                .size(10.5)
                                .color(Palette::TEXT_MUTED),
                        );
                    });
                });
            });

        if click_start_update {
            self.trigger_git_self_update(ctx);
        }
        if let Some(exe) = click_relaunch_exe {
            match UpdateService::relaunch_binary(&exe) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => self.status_text = e,
            }
        }

        // 3. Left Architectural Navigation Rail
        egui::SidePanel::left("left_nav_rail")
            .exact_width(218.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(Palette::BG_VOID)
                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                    .inner_margin(Margin::ZERO),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("NAVIGATION")
                            .monospace()
                            .strong()
                            .size(10.0)
                            .color(Palette::TEXT_MUTED),
                    );
                });
                ui.add_space(6.0);

                let lua_n = self.recent_installed.len();
                let lib_lbl = if lua_n > 0 {
                    format!("03  LIBRARY  [{}]", lua_n)
                } else {
                    "03  LIBRARY".into()
                };

                let items = [
                    ("01  OVERVIEW".to_string(), ActiveView::Overview),
                    ("02  ACQUIRE".to_string(), ActiveView::Acquire),
                    (lib_lbl, ActiveView::Library),
                    ("04  DEPOTS & VAULT".to_string(), ActiveView::Depots),
                    ("05  FIX CATALOG".to_string(), ActiveView::Fixes),
                    ("06  TOOLCHAIN".to_string(), ActiveView::Toolchain),
                    ("07  SYSTEM".to_string(), ActiveView::Settings),
                ];

                for (lbl, view) in items {
                    if nav_rail_button(ui, &lbl, self.active_view == view, 218.0).clicked() {
                        self.active_view = view;
                    }
                }

                // Bottom Telemetry Block
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    Frame::new()
                        .fill(egui::Color32::from_rgb(12, 12, 15))
                        .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(194.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.label(
                                    RichText::new("TELEMETRY")
                                        .monospace()
                                        .strong()
                                        .size(10.0)
                                        .color(Palette::BLUE_LIGHT),
                                );
                                let auth_txt = if self.auth_service.is_signed_in() {
                                    format!("AUTH     :: {}", self.auth_service.display_name())
                                } else {
                                    "AUTH     :: GUEST".into()
                                };
                                ui.label(RichText::new(auth_txt).monospace().size(10.0).color(
                                    if self.auth_service.is_signed_in() {
                                        Palette::BLUE_LIGHT
                                    } else {
                                        Palette::TEXT_MUTED
                                    },
                                ));
                                let sls_txt = if self.slssteam_service.config.exists() {
                                    format!(
                                        "SLSSTEAM :: PNO={}",
                                        if self.slssteam_service.config.play_not_owned {
                                            "ON"
                                        } else {
                                            "OFF"
                                        }
                                    )
                                } else {
                                    "SLSSTEAM :: UNSET".into()
                                };
                                ui.label(
                                    RichText::new(sls_txt)
                                        .monospace()
                                        .size(10.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                let br_txt = if self.millennium_connected {
                                    "BRIDGE   :: RPC :38495"
                                } else if self.cef_injector.is_running() {
                                    "BRIDGE   :: CEF :8080"
                                } else {
                                    "BRIDGE   :: DIRECT"
                                };
                                ui.label(RichText::new(br_txt).monospace().size(10.0).color(
                                    if self.millennium_connected {
                                        Palette::BLUE_LIGHT
                                    } else {
                                        Palette::TEXT_MUTED
                                    },
                                ));
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("DROP .LUA / .ZIP ANYWHERE")
                                        .monospace()
                                        .size(9.5)
                                        .color(Palette::TEXT_MUTED),
                                );
                            });
                        });
                });
            });

        // 4. Central Workspace Panel
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(Palette::BG_BASE)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ctx, |ui| match self.active_view {
                ActiveView::Overview => self.render_overview(ui),
                ActiveView::Acquire => self.render_acquire(ui),
                ActiveView::Library => self.render_library(ui),
                ActiveView::Depots => self.render_depots(ui),
                ActiveView::Fixes => self.render_fixes(ui),
                ActiveView::Toolchain => self.render_toolchain(ui),
                ActiveView::Settings => self.render_settings(ui),
            });

        // 5. Drag-and-Drop Hover Banner
        let hovering_files = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering_files {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(
                screen,
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgba_unmultiplied(15, 26, 48, 210),
            );
            painter.rect_stroke(
                screen.shrink(12.0),
                egui::CornerRadius::ZERO,
                Stroke::new(2.0_f32, Palette::BLUE_HOVER),
                egui::StrokeKind::Inside,
            );
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "DROP .LUA / .MANIFEST / .ZIP PACKAGE TO INSTALL",
                egui::FontId::monospace(18.0),
                Palette::BLUE_LIGHT,
            );
        }

        // 6. Modals
        self.render_modals(ctx);
    }
}
