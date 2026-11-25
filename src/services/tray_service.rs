//! TrayService — Native Linux system tray icon (AppIndicator3 / StatusNotifierItem via runtime `dlopen`)
//! with desktop host verification, context menu dispatch, desktop notifications, and safe fallback.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::models::config::default_luatools_cache_dir;
use crate::ui::state::AppMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    ShowWindow,
    HideWindow,
    ToggleWindow,
    Quit,
    RestartSteam,
    OpenSettings,
}

#[derive(Clone)]
struct SharedTrayDispatch {
    app_message_tx: Arc<Mutex<Option<Sender<AppMessage>>>>,
    egui_ctx: Arc<Mutex<Option<egui::Context>>>,
    is_visible: Arc<Mutex<bool>>,
}

impl SharedTrayDispatch {
    fn emit(&self, event: TrayEvent) {
        let msg = match event {
            TrayEvent::ShowWindow => {
                *self.is_visible.lock().unwrap_or_else(|e| e.into_inner()) = true;
                AppMessage::ShowWindow
            }
            TrayEvent::HideWindow => {
                *self.is_visible.lock().unwrap_or_else(|e| e.into_inner()) = false;
                AppMessage::HideWindow
            }
            TrayEvent::ToggleWindow => {
                let mut vis = self.is_visible.lock().unwrap_or_else(|e| e.into_inner());
                if *vis {
                    *vis = false;
                    AppMessage::HideWindow
                } else {
                    *vis = true;
                    AppMessage::ShowWindow
                }
            }
            TrayEvent::OpenSettings => {
                *self.is_visible.lock().unwrap_or_else(|e| e.into_inner()) = true;
                AppMessage::OpenSettings
            }
            TrayEvent::RestartSteam => AppMessage::RestartSteamRequested,
            TrayEvent::Quit => AppMessage::QuitRequested,
        };

        if let Some(tx) = self
            .app_message_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = tx.send(msg);
        }
        if let Some(ctx) = self
            .egui_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            ctx.request_repaint();
        }
    }
}

enum WorkerCommand {
    Activate {
        dispatch: SharedTrayDispatch,
        reply: Sender<Result<(), String>>,
    },
    SetTitle(String),
    Deactivate,
}

const APP_INDICATOR_CATEGORY_APPLICATION_STATUS: c_int = 0;
const APP_INDICATOR_STATUS_PASSIVE: c_int = 0;
const APP_INDICATOR_STATUS_ACTIVE: c_int = 1;

struct MenuCallbackData {
    dispatch: Arc<Mutex<SharedTrayDispatch>>,
    event: TrayEvent,
}

unsafe extern "C" fn on_gtk_menu_item_activate(_widget: *mut c_void, user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let cb = &*(user_data as *const MenuCallbackData);
    let dispatch = cb
        .dispatch
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    dispatch.emit(cb.event);
}

struct DynamicIndicatorFns {
    gtk_init_check: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int,
    gtk_events_pending: unsafe extern "C" fn() -> c_int,
    gtk_main_iteration_do: unsafe extern "C" fn(c_int) -> c_int,
    gtk_menu_new: unsafe extern "C" fn() -> *mut c_void,
    gtk_menu_item_new_with_label: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    gtk_separator_menu_item_new: unsafe extern "C" fn() -> *mut c_void,
    gtk_menu_shell_append: unsafe extern "C" fn(*mut c_void, *mut c_void),
    gtk_widget_show_all: unsafe extern "C" fn(*mut c_void),
    g_signal_connect_data: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        unsafe extern "C" fn(*mut c_void, *mut c_void),
        *mut c_void,
        *const c_void,
        c_uint,
    ) -> c_ulong,
    app_indicator_new_with_path:
        unsafe extern "C" fn(*const c_char, *const c_char, c_int, *const c_char) -> *mut c_void,
    app_indicator_set_status: unsafe extern "C" fn(*mut c_void, c_int),
    app_indicator_set_title: unsafe extern "C" fn(*mut c_void, *const c_char),
    app_indicator_set_menu: unsafe extern "C" fn(*mut c_void, *mut c_void),
    app_indicator_set_secondary_activate_target: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

unsafe fn dlopen_any(candidates: &[&str]) -> *mut c_void {
    for name in candidates {
        if let Ok(c_name) = CString::new(*name) {
            let h = libc::dlopen(c_name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if !h.is_null() {
                return h;
            }
        }
    }
    ptr::null_mut()
}

unsafe fn dlsym_checked(handle: *mut c_void, symbol: &str) -> Result<*mut c_void, String> {
    let c_sym = CString::new(symbol).map_err(|e| e.to_string())?;
    let ptr = libc::dlsym(handle, c_sym.as_ptr());
    if ptr.is_null() {
        Err(format!("Missing symbol {}", symbol))
    } else {
        Ok(ptr)
    }
}

impl DynamicIndicatorFns {
    unsafe fn load() -> Result<Self, String> {
        let ind_lib = dlopen_any(&[
            "libayatana-appindicator3.so.1",
            "libappindicator3.so.1",
        ]);
        if ind_lib.is_null() {
            return Err(
                "Neither libayatana-appindicator3.so.1 nor libappindicator3.so.1 is installed"
                    .to_string(),
            );
        }
        let gtk_lib = dlopen_any(&["libgtk-3.so.0"]);
        if gtk_lib.is_null() {
            return Err("libgtk-3.so.0 is not available".to_string());
        }
        let gobj_lib = dlopen_any(&["libgobject-2.0.so.0"]);
        if gobj_lib.is_null() {
            return Err("libgobject-2.0.so.0 is not available".to_string());
        }

        Ok(Self {
            gtk_init_check: std::mem::transmute(dlsym_checked(gtk_lib, "gtk_init_check")?),
            gtk_events_pending: std::mem::transmute(dlsym_checked(gtk_lib, "gtk_events_pending")?),
            gtk_main_iteration_do: std::mem::transmute(dlsym_checked(
                gtk_lib,
                "gtk_main_iteration_do",
            )?),
            gtk_menu_new: std::mem::transmute(dlsym_checked(gtk_lib, "gtk_menu_new")?),
            gtk_menu_item_new_with_label: std::mem::transmute(dlsym_checked(
                gtk_lib,
                "gtk_menu_item_new_with_label",
            )?),
            gtk_separator_menu_item_new: std::mem::transmute(dlsym_checked(
                gtk_lib,
                "gtk_separator_menu_item_new",
            )?),
            gtk_menu_shell_append: std::mem::transmute(dlsym_checked(
                gtk_lib,
                "gtk_menu_shell_append",
            )?),
            gtk_widget_show_all: std::mem::transmute(dlsym_checked(
                gtk_lib,
                "gtk_widget_show_all",
            )?),
            g_signal_connect_data: std::mem::transmute(dlsym_checked(
                gobj_lib,
                "g_signal_connect_data",
            )?),
            app_indicator_new_with_path: std::mem::transmute(dlsym_checked(
                ind_lib,
                "app_indicator_new_with_path",
            )?),
            app_indicator_set_status: std::mem::transmute(dlsym_checked(
                ind_lib,
                "app_indicator_set_status",
            )?),
            app_indicator_set_title: std::mem::transmute(dlsym_checked(
                ind_lib,
                "app_indicator_set_title",
            )?),
            app_indicator_set_menu: std::mem::transmute(dlsym_checked(
                ind_lib,
                "app_indicator_set_menu",
            )?),
            app_indicator_set_secondary_activate_target: std::mem::transmute(dlsym_checked(
                ind_lib,
                "app_indicator_set_secondary_activate_target",
            )?),
        })
    }
}

fn tray_worker_sender() -> Result<&'static Sender<WorkerCommand>, String> {
    static WORKER: OnceLock<Result<Sender<WorkerCommand>, String>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCommand>();
            let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

            let icon_dir = default_luatools_cache_dir().join("tray");
            let _ = TrayService::write_tray_icon_png(&icon_dir.join("luatools-tray.png"));

            let spawn_res = thread::Builder::new()
                .name("luatools-tray".to_string())
                .spawn(move || unsafe {
                    let fns = match DynamicIndicatorFns::load() {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = init_tx.send(Err(e));
                            return;
                        }
                    };

                    if (fns.gtk_init_check)(ptr::null_mut(), ptr::null_mut()) == 0 {
                        let _ = init_tx
                            .send(Err("gtk_init_check failed (no graphical display)".into()));
                        return;
                    }

                    let c_id = CString::new("luatools-linux").unwrap();
                    let c_icon = CString::new("luatools-tray").unwrap();
                    let c_path =
                        CString::new(icon_dir.to_string_lossy().as_bytes()).unwrap_or_default();
                    let c_title = CString::new("LuaTools Linux").unwrap();
                    let c_activate = CString::new("activate").unwrap();

                    let indicator = (fns.app_indicator_new_with_path)(
                        c_id.as_ptr(),
                        c_icon.as_ptr(),
                        APP_INDICATOR_CATEGORY_APPLICATION_STATUS,
                        c_path.as_ptr(),
                    );
                    if indicator.is_null() {
                        let _ = init_tx.send(Err("app_indicator_new_with_path returned null".into()));
                        return;
                    }

                    (fns.app_indicator_set_title)(indicator, c_title.as_ptr());

                    let shared_dispatch = Arc::new(Mutex::new(SharedTrayDispatch {
                        app_message_tx: Arc::new(Mutex::new(None)),
                        egui_ctx: Arc::new(Mutex::new(None)),
                        is_visible: Arc::new(Mutex::new(true)),
                    }));

                    let menu = (fns.gtk_menu_new)();
                    let mut callbacks: Vec<Box<MenuCallbackData>> = Vec::new();
                    let mut add_item =
                        |label: &str, event: TrayEvent| -> *mut c_void {
                            let c_lbl = CString::new(label).unwrap();
                            let item = (fns.gtk_menu_item_new_with_label)(c_lbl.as_ptr());
                            let cb = Box::new(MenuCallbackData {
                                dispatch: Arc::clone(&shared_dispatch),
                                event,
                            });
                            let cb_ptr = (&*cb as *const MenuCallbackData) as *mut c_void;
                            callbacks.push(cb);
                            (fns.g_signal_connect_data)(
                                item,
                                c_activate.as_ptr(),
                                on_gtk_menu_item_activate,
                                cb_ptr,
                                ptr::null(),
                                0,
                            );
                            (fns.gtk_menu_shell_append)(menu, item);
                            item
                        };

                    let toggle_item = add_item("Show / Hide LuaTools", TrayEvent::ToggleWindow);
                    let _ = add_item("Show LuaTools", TrayEvent::ShowWindow);
                    let _ = add_item("Hide to Tray", TrayEvent::HideWindow);
                    (fns.gtk_menu_shell_append)(menu, (fns.gtk_separator_menu_item_new)());
                    let _ = add_item("Restart Steam with Bridge", TrayEvent::RestartSteam);
                    let _ = add_item("Settings", TrayEvent::OpenSettings);
                    (fns.gtk_menu_shell_append)(menu, (fns.gtk_separator_menu_item_new)());
                    let _ = add_item("Quit", TrayEvent::Quit);

                    (fns.gtk_widget_show_all)(menu);
                    (fns.app_indicator_set_menu)(indicator, menu);
                    (fns.app_indicator_set_secondary_activate_target)(indicator, toggle_item);

                    let _ = init_tx.send(Ok(()));

                    loop {
                        while let Ok(cmd) = cmd_rx.try_recv() {
                            match cmd {
                                WorkerCommand::Activate { dispatch, reply } => {
                                    *shared_dispatch.lock().unwrap_or_else(|e| e.into_inner()) =
                                        dispatch;
                                    (fns.app_indicator_set_status)(
                                        indicator,
                                        APP_INDICATOR_STATUS_ACTIVE,
                                    );
                                    let _ = reply.send(Ok(()));
                                }
                                WorkerCommand::SetTitle(text) => {
                                    if let Ok(c_txt) = CString::new(text) {
                                        (fns.app_indicator_set_title)(indicator, c_txt.as_ptr());
                                    }
                                }
                                WorkerCommand::Deactivate => {
                                    (fns.app_indicator_set_status)(
                                        indicator,
                                        APP_INDICATOR_STATUS_PASSIVE,
                                    );
                                }
                            }
                        }

                        while (fns.gtk_events_pending)() != 0 {
                            (fns.gtk_main_iteration_do)(0);
                        }

                        thread::sleep(Duration::from_millis(40));
                    }
                });

            if let Err(e) = spawn_res {
                return Err(format!("Failed to spawn tray thread: {}", e));
            }

            match init_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Ok(())) => Ok(cmd_tx),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("Timed out initializing GTK/AppIndicator tray worker".to_string()),
            }
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub struct TrayService {
    app_message_tx: Arc<Mutex<Option<Sender<AppMessage>>>>,
    egui_ctx: Arc<Mutex<Option<egui::Context>>>,
    is_visible: Arc<Mutex<bool>>,
    tray_available: Arc<AtomicBool>,
    owns_worker_registration: bool,
    tooltip_cache: Mutex<String>,
}

impl TrayService {
    pub fn new() -> Self {
        Self {
            app_message_tx: Arc::new(Mutex::new(None)),
            egui_ctx: Arc::new(Mutex::new(None)),
            is_visible: Arc::new(Mutex::new(true)),
            tray_available: Arc::new(AtomicBool::new(false)),
            owns_worker_registration: false,
            tooltip_cache: Mutex::new("LuaTools Linux".to_string()),
        }
    }

    /// Creates an offline/test instance with a deterministic tray availability flag.
    pub fn new_with_availability(available: bool) -> Self {
        let svc = Self::new();
        svc.tray_available.store(available, Ordering::SeqCst);
        svc
    }

    pub fn set_app_message_sender(&mut self, tx: Sender<AppMessage>) {
        *self
            .app_message_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    pub fn set_egui_context(&mut self, ctx: egui::Context) {
        *self.egui_ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx);
    }

    pub fn set_visibility(&self, visible: bool) {
        *self.is_visible.lock().unwrap_or_else(|e| e.into_inner()) = visible;
    }

    pub fn is_visible(&self) -> bool {
        *self.is_visible.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns `true` only when a live desktop tray host is present AND the tray indicator initialized cleanly.
    pub fn is_tray_available(&self) -> bool {
        self.tray_available.load(Ordering::SeqCst)
    }

    pub fn set_tray_available_for_test(&self, available: bool) {
        self.tray_available.store(available, Ordering::SeqCst);
    }

    /// Simulates a tray menu event directly (used by unit tests and programmatic triggers).
    pub fn dispatch_event(&self, event: TrayEvent) {
        let dispatch = SharedTrayDispatch {
            app_message_tx: Arc::clone(&self.app_message_tx),
            egui_ctx: Arc::clone(&self.egui_ctx),
            is_visible: Arc::clone(&self.is_visible),
        };
        dispatch.emit(event);
    }

    /// Detects whether the current desktop session has a live StatusNotifierHost or X11 system tray.
    pub fn has_live_tray_host() -> bool {
        let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if !has_display {
            return false;
        }

        // 1. Check org.kde.StatusNotifierWatcher / org.freedesktop.StatusNotifierWatcher via busctl or gdbus
        for dest in [
            "org.kde.StatusNotifierWatcher",
            "org.freedesktop.StatusNotifierWatcher",
        ] {
            if let Ok(out) = Command::new("busctl")
                .args([
                    "--user",
                    "get-property",
                    dest,
                    "/StatusNotifierWatcher",
                    dest,
                    "IsStatusNotifierHostRegistered",
                ])
                .output()
            {
                if out.status.success()
                    && String::from_utf8_lossy(&out.stdout)
                        .to_ascii_lowercase()
                        .contains("true")
                {
                    return true;
                }
            }

            if let Ok(out) = Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest",
                    dest,
                    "--object-path",
                    "/StatusNotifierWatcher",
                    "--method",
                    "org.freedesktop.DBus.Properties.Get",
                    dest,
                    "IsStatusNotifierHostRegistered",
                ])
                .output()
            {
                if out.status.success()
                    && String::from_utf8_lossy(&out.stdout)
                        .to_ascii_lowercase()
                        .contains("true")
                {
                    return true;
                }
            }
        }

        // 2. Fallback: Check X11 _NET_SYSTEM_TRAY_S0 selection owner via libX11.so.6 if DISPLAY is set
        if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
            if unsafe { Self::has_x11_tray_owner() } {
                return true;
            }
        }

        false
    }

    unsafe fn has_x11_tray_owner() -> bool {
        let x11 = dlopen_any(&["libX11.so.6"]);
        if x11.is_null() {
            return false;
        }
        let Ok(open_ptr) = dlsym_checked(x11, "XOpenDisplay") else {
            return false;
        };
        let Ok(atom_ptr) = dlsym_checked(x11, "XInternAtom") else {
            return false;
        };
        let Ok(owner_ptr) = dlsym_checked(x11, "XGetSelectionOwner") else {
            return false;
        };
        let Ok(close_ptr) = dlsym_checked(x11, "XCloseDisplay") else {
            return false;
        };

        let x_open: unsafe extern "C" fn(*const c_char) -> *mut c_void =
            std::mem::transmute(open_ptr);
        let x_atom: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_ulong =
            std::mem::transmute(atom_ptr);
        let x_owner: unsafe extern "C" fn(*mut c_void, c_ulong) -> c_ulong =
            std::mem::transmute(owner_ptr);
        let x_close: unsafe extern "C" fn(*mut c_void) -> c_int = std::mem::transmute(close_ptr);

        let dpy = x_open(ptr::null());
        if dpy.is_null() {
            return false;
        }
        let sel_name = CString::new("_NET_SYSTEM_TRAY_S0").unwrap();
        let atom = x_atom(dpy, sel_name.as_ptr(), 0);
        let owner = if atom != 0 { x_owner(dpy, atom) } else { 0 };
        let _ = x_close(dpy);
        owner != 0
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        if !Self::has_live_tray_host() {
            self.tray_available.store(false, Ordering::SeqCst);
            return Err(
                "No active desktop system tray host (StatusNotifierHost / XEmbed) detected"
                    .to_string(),
            );
        }

        let worker_tx = tray_worker_sender()?;
        let (reply_tx, reply_rx) = mpsc::channel();
        let dispatch = SharedTrayDispatch {
            app_message_tx: Arc::clone(&self.app_message_tx),
            egui_ctx: Arc::clone(&self.egui_ctx),
            is_visible: Arc::clone(&self.is_visible),
        };
        worker_tx
            .send(WorkerCommand::Activate {
                dispatch,
                reply: reply_tx,
            })
            .map_err(|_| "Tray worker channel disconnected".to_string())?;

        match reply_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => {
                self.tray_available.store(true, Ordering::SeqCst);
                self.owns_worker_registration = true;
                Ok(())
            }
            Ok(Err(e)) => {
                self.tray_available.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => {
                self.tray_available.store(false, Ordering::SeqCst);
                Err("Timed out activating system tray indicator".to_string())
            }
        }
    }

    /// Generates the 64x64 RGBA icon buffer used for the tray icon PNG.
    pub fn build_tray_icon_rgba() -> (Vec<u8>, u32, u32) {
        let w = 64usize;
        let h = 64usize;
        let mut rgba = vec![0u8; w * h * 4];

        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) * 4;
                let mut color = [0u8, 0, 0, 0];
                if (3..61).contains(&x) && (3..61).contains(&y) {
                    color = [24, 26, 32, 255]; // #181A20 paper card
                }
                if (3..8).contains(&x) && (3..61).contains(&y) {
                    color = [26, 58, 107, 255]; // #1A3A6B deep sapphire
                }
                if ((16..23).contains(&x) && (17..48).contains(&y))
                    || ((16..34).contains(&x) && (41..48).contains(&y))
                {
                    color = [239, 240, 244, 255]; // crisp 'L'
                }
                if ((32..53).contains(&x) && (17..24).contains(&y))
                    || ((39..46).contains(&x) && (17..48).contains(&y))
                {
                    color = [118, 162, 230, 255]; // 'T'
                }
                rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }

        (rgba, w as u32, h as u32)
    }

    pub fn write_tray_icon_png(dest: &Path) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let (rgba, w, h) = Self::build_tray_icon_rgba();
        image::save_buffer(dest, &rgba, w, h, image::ColorType::Rgba8).map_err(|e| e.to_string())
    }

    pub fn update_tooltip(&self, text: &str) {
        *self.tooltip_cache.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
        if self.is_tray_available() {
            if let Ok(tx) = tray_worker_sender() {
                let _ = tx.send(WorkerCommand::SetTitle(text.to_string()));
            }
        }
    }

    pub fn tooltip(&self) -> String {
        self.tooltip_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn show_notification(&self, title: &str, body: &str) {
        let has_display = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if !has_display {
            return;
        }
        let title_owned = title.to_string();
        let body_owned = body.to_string();
        let icon_path = default_luatools_cache_dir()
            .join("tray")
            .join("luatools-tray.png");
        thread::spawn(move || {
            let mut cmd = Command::new("notify-send");
            cmd.arg("--app-name=LuaTools Linux");
            if icon_path.is_file() {
                cmd.arg(format!("--icon={}", icon_path.display()));
            }
            let _ = cmd.arg("--").arg(&title_owned).arg(&body_owned).status();
        });
    }

    pub fn cleanup(&mut self) {
        self.tray_available.store(false, Ordering::SeqCst);
        if self.owns_worker_registration {
            self.owns_worker_registration = false;
            if let Ok(tx) = tray_worker_sender() {
                let _ = tx.send(WorkerCommand::Deactivate);
            }
        }
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl Default for TrayService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_icon_rgba_and_png_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let png_path = tmp.path().join("tray/luatools-tray.png");
        TrayService::write_tray_icon_png(&png_path).expect("write tray icon png");
        assert!(png_path.is_file());
        let meta = fs::metadata(&png_path).unwrap();
        assert!(meta.len() > 100);
    }

    #[test]
    fn test_tray_event_dispatch_and_toggle_visibility() {
        let (tx, rx) = mpsc::channel();
        let mut svc = TrayService::new_with_availability(true);
        svc.set_app_message_sender(tx);
        assert!(svc.is_visible());
        assert!(svc.is_tray_available());

        // First toggle hides window
        svc.dispatch_event(TrayEvent::ToggleWindow);
        assert!(!svc.is_visible());
        assert!(matches!(rx.try_recv().unwrap(), AppMessage::HideWindow));

        // Second toggle restores window
        svc.dispatch_event(TrayEvent::ToggleWindow);
        assert!(svc.is_visible());
        assert!(matches!(rx.try_recv().unwrap(), AppMessage::ShowWindow));

        // OpenSettings marks visible and dispatches OpenSettings
        svc.set_visibility(false);
        svc.dispatch_event(TrayEvent::OpenSettings);
        assert!(svc.is_visible());
        assert!(matches!(rx.try_recv().unwrap(), AppMessage::OpenSettings));

        // Quit dispatches QuitRequested
        svc.dispatch_event(TrayEvent::Quit);
        assert!(matches!(rx.try_recv().unwrap(), AppMessage::QuitRequested));

        svc.update_tooltip("LuaTools · 3 games");
        assert_eq!(svc.tooltip(), "LuaTools · 3 games");

        svc.cleanup();
        assert!(!svc.is_tray_available());
    }
}
