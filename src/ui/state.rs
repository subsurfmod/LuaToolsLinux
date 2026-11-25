//! Shared application view models, navigation enums, modal states, and async worker messages.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::parsers::lua_parser::LuaEntry;
use crate::parsers::vdf_binary::LaunchEntry;
use crate::services::api_client::{
    DRMFixFixItem, DRMFixGameListing, DRMFixTag, DlcInfo, SteamFeaturedItem, SteamSearchItem,
};
use crate::services::caches::GameDetails;
use crate::services::depot_downloader::DepotRunResult;
use crate::services::depot_info::{AppDepotInfo, ContentDepot};
use crate::services::update_service::{UpdateApplyResult, UpdateCheckResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
}

#[derive(Debug, Clone)]
pub struct ToastNotification {
    pub text: String,
    pub kind: ToastKind,
    pub created_at: std::time::Instant,
    pub duration_secs: f32,
}

#[derive(Debug, Clone)]
pub enum UpdateBannerState {
    Hidden,
    Available {
        badge: String,
        tooltip: String,
    },
    Updating {
        pct: f32,
        step: String,
    },
    ReadyToRestart {
        new_version: String,
        exe_path: PathBuf,
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Overview,
    Acquire,
    Library,
    Depots,
    Fixes,
    Guide,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideTopic {
    QuickStart,
    AddingGames,
    DepotsAndVault,
    FixesAndOverrides,
    SteamInterop,
    Troubleshooting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireTab {
    TopSellers,
    NewReleases,
    SearchResults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepotsSubTab {
    Declarations,
    LiveEditor,
    DownloadPicker,
    ManifestBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSubTab {
    General,
    Account,
    Integrations,
    SteamIntegration,
    UpdatesAndTools,
    Telemetry,
}

#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub app_id: u32,
    pub name: String,
    pub installed: bool,
    pub has_lua: bool,
    pub fake_id: Option<u32>,
    pub has_token: bool,
    pub status_label: &'static str,
    pub install_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DepotPickRow {
    pub depot: ContentDepot,
    pub lua_entry: Option<LuaEntry>,
    pub manifest_id: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub has_key: bool,
    pub size: u64,
    pub blocked_reason: Option<String>,
    pub is_selected: bool,
}

pub enum AppMessage {
    BridgeProbed {
        available: bool,
        version: Option<String>,
        steam_path: Option<String>,
    },
    CoverLoaded {
        app_id: u32,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    CoverFailed {
        app_id: u32,
    },
    FeaturedLoaded {
        top: Vec<SteamFeaturedItem>,
        new_releases: Vec<SteamFeaturedItem>,
    },
    SearchFinished {
        items: Vec<SteamSearchItem>,
        error: Option<String>,
    },
    AcquireDetailsLoaded {
        app_id: u32,
        details: Option<GameDetails>,
    },
    AcquireSourcesLoaded {
        app_id: u32,
        sources: BTreeMap<String, String>,
        dlc_info: Option<DlcInfo>,
    },
    AcquireProgress {
        pct: f32,
        msg: String,
    },
    AcquireInstallFinished {
        app_id: u32,
        ok: bool,
        downloaded_path: Option<PathBuf>,
        source: String,
        msg: String,
    },
    DepotInfoLoaded {
        app_id: u32,
        info: Option<AppDepotInfo>,
    },
    DepotDownloadProgress {
        depot_label: String,
        pct: f32,
        msg: String,
    },
    DepotDownloadFinished {
        results: Vec<DepotRunResult>,
    },
    FixListingsLoaded {
        games: Vec<DRMFixGameListing>,
        tags: Vec<DRMFixTag>,
    },
    FixesLoaded {
        app_id: u32,
        fixes: Vec<DRMFixFixItem>,
    },
    FixProgress {
        pct: f32,
        msg: String,
    },
    FixActionFinished {
        app_id: u32,
        ok: bool,
        downloaded_manifest: Option<PathBuf>,
        fix_id: String,
        msg: String,
    },
    ToolProgress {
        tool_key: String,
        pct: f32,
        msg: String,
    },
    ToolFinished {
        tool_key: String,
        ok: bool,
        msg: String,
    },
    AuthCompleted {
        ok: bool,
        msg: String,
    },
    UsageLoaded(String),
    HubcapChecked {
        ok: bool,
        msg: String,
    },
    DonatedKeys(usize),
    SteamRestarted {
        ok: bool,
        msg: String,
    },
    FilesPickedForIngest(Vec<PathBuf>),
    FolderPickedForDepotDest(PathBuf),
    FolderPickedForSteamOverride(PathBuf),
    FilePickedForWorkshop(PathBuf),
    FolderPickedForAutoCrack(PathBuf),
    FilePickedForStubRemover(PathBuf),
    UpdateCheckFinished(UpdateCheckResult),
    UpdateProgress {
        pct: f32,
        msg: String,
    },
    UpdateApplyFinished(Result<UpdateApplyResult, String>),
    FolderPickedForUpdateRepo(PathBuf),
    TelemetryEndpointTested {
        ok: bool,
        msg: String,
    },
    FolderPickedForDiagExport(PathBuf),
    WiredProgress {
        pct: f32,
        msg: String,
    },
    WiredFinished {
        ok: bool,
        msg: String,
    },
    BridgeActionFinished {
        ok: bool,
        msg: String,
    },
    GameIntegrityChecked {
        app_id: u32,
        checked: usize,
        ok: bool,
        msg: String,
    },
    FilePickedForGameInjectionSo(PathBuf),
    LaunchOptionsFinished {
        app_id: u32,
        ok: bool,
        entries: Option<Vec<LaunchEntry>>,
        message: String,
        warn: bool,
    },
    ShowWindow,
    HideWindow,
    OpenSettings,
    RestartSteamRequested,
    QuitRequested,
}

// ===== Modal Dialog States =====

#[derive(Debug, Clone, Default)]
pub struct AddByAppIdModal {
    pub open: bool,
    pub app_id_input: String,
    pub title_input: String,
    pub template_idx: usize,
}

#[derive(Debug, Clone, Default)]
pub struct GameDetailModal {
    pub open: bool,
    pub app_id: u32,
    pub game_name: String,
    pub lua_text: String,
    pub fake_id_input: String,
    pub token_input: String,
    pub show_token: bool,
    pub quick_depot_input: String,
    pub feedback: String,
}

#[derive(Debug, Clone, Default)]
pub struct IntegrationModal {
    pub open: bool,
    pub app_id: u32,
    pub game_name: String,
    pub install_path: Option<PathBuf>,
    pub spoof_id_input: String,
    pub cd_key_input: String,
    pub depot_id_input: String,
    pub depot_key_input: String,
    pub stat_name_input: String,
    pub stat_value_input: String,
    pub proc_enabled: bool,
    pub proc_lib_input: String,
    pub proc_args_input: String,
    pub feedback: String,
}

#[derive(Debug, Clone, Default)]
pub struct LaunchOptionsModal {
    pub open: bool,
    pub app_id: u32,
    pub game_name: String,
    pub entries: Vec<LaunchEntry>,
    pub selected_idx: Option<usize>,
    pub feedback: String,
    pub steam_running: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DepotDetailModal {
    pub open: bool,
    pub depot: Option<ContentDepot>,
    pub entry: Option<LuaEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct TextPromptModal {
    pub open: bool,
    pub kind: String, // "preset" | "rename_variant" | "bot_code"
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub target_hash: String,
}

#[derive(Debug, Clone)]
pub struct QaCaptureState {
    pub out_dir: PathBuf,
    pub step: usize,
    pub frames_waited: u32,
    pub pending_save: Option<String>,
}
