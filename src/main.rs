//! Entry point for LuaTools Linux native desktop application.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use eframe::egui;
use luatools_linux::services::protocol_service::ProtocolService;
use luatools_linux::services::settings_service::SettingsService;
use luatools_linux::services::steam_service::detect_steam_install;
use luatools_linux::services::update_service::UpdateService;
use luatools_linux::steam_bridge::ost_platform::{dialog, trap};
use luatools_linux::ui::app::LuaToolsApp;
use luatools_linux::VERSION;

fn has_display_session() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

fn report_fatal_error(title: &str, message: &str, allow_dialog: bool) {
    eprintln!("ERROR :: {}", message);
    if allow_dialog && has_display_session() {
        let _ = dialog::show_warning(title, message);
    }
}

enum IpcCommand {
    Focus,
    TrayLock,
    Url(String),
}

struct InstanceLock {
    _file: std::fs::File,
}

impl InstanceLock {
    fn try_acquire(path: &Path) -> Result<Self, ()> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| ())?;

        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            Ok(Self { _file: file })
        } else {
            Err(())
        }
    }
}

fn get_instance_paths() -> (PathBuf, PathBuf) {
    let base = dirs::runtime_dir()
        .or_else(|| dirs::cache_dir().map(|c| c.join("luatools")))
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&base);
    let uid = unsafe { libc::getuid() };
    (
        base.join(format!("luatools_instance_{}.lock", uid)),
        base.join(format!("luatools_instance_{}.sock", uid)),
    )
}

struct AppWrapper {
    inner: LuaToolsApp,
    ipc_rx: Receiver<IpcCommand>,
    tray_locked: bool,
    initial_minimized: bool,
    sock_path: PathBuf,
    _instance_lock: InstanceLock,
}

impl Drop for AppWrapper {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

impl eframe::App for AppWrapper {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.initial_minimized {
            self.initial_minimized = false;
            if self.inner.can_hide_to_tray() {
                self.inner.tray_service.set_visibility(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                // No system tray host available — ensure window is visible in taskbar (minimized), never headless!
                self.inner.tray_service.set_visibility(true);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }

        let mut had_ipc = false;
        while let Ok(cmd) = self.ipc_rx.try_recv() {
            had_ipc = true;
            match cmd {
                IpcCommand::Focus => {
                    self.inner.restore_from_tray(ctx);
                }
                IpcCommand::TrayLock => {
                    self.tray_locked = true;
                    self.inner.tray_locked = true;
                }
                IpcCommand::Url(url) => {
                    let is_silent = ProtocolService::parse(&url)
                        .map(|a| a.silent)
                        .unwrap_or(false);
                    if !is_silent {
                        self.inner.restore_from_tray(ctx);
                    }
                    self.inner.handle_protocol_url(&url, ctx);
                }
            }
        }
        if had_ipc {
            ctx.request_repaint();
        }

        if ctx.input(|i| i.viewport().close_requested())
            && self.inner.should_intercept_close_to_tray(self.tray_locked)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.inner.hide_to_tray(ctx);
        }

        self.inner.update(ctx, frame);
    }
}

fn main() -> eframe::Result<()> {
    let main_exe_path = trap::get_main_executable_path().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_protocol_url: Option<String> = None;
    let mut qa_screenshots_dir: Option<PathBuf> = None;
    let mut minimized = false;
    let mut tray_locked = false;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "-v" | "--version" => {
                println!("luatools-linux v{}", VERSION);
                return Ok(());
            }
            "--check" => {
                let exe_label = main_exe_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let steam = detect_steam_install(None);
                match steam {
                    Some(inst) => println!(
                        "OK :: LuaTools Linux v{} · Exe={} · Steam={} ({} libraries)",
                        VERSION,
                        exe_label,
                        inst.root.display(),
                        inst.libraries.len()
                    ),
                    None => println!(
                        "OK :: LuaTools Linux v{} · Exe={} · Steam=not detected (standalone ready)",
                        VERSION,
                        exe_label
                    ),
                }
                return Ok(());
            }
            "--update" => {
                let settings = SettingsService::default().settings;
                println!(
                    "LuaTools Linux v{} — Pulling from Git and updating...",
                    VERSION
                );
                match UpdateService::pull_and_update(
                    &settings.update_repo_dir,
                    &settings.git_remote_url,
                    |pct, msg| println!("[{:>3.0}%] {}", pct * 100.0, msg),
                ) {
                    Ok(res) => {
                        println!(
                            "SUCCESS :: Updated to v{} ({}) -> {}",
                            res.new_version,
                            res.commit_info,
                            res.updated_exe.display()
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        report_fatal_error("LuaTools Linux — Update Error", &e, true);
                        std::process::exit(1);
                    }
                }
            }
            "--qa-screenshots" => {
                if let Some(dir) = args.get(idx + 1) {
                    qa_screenshots_dir = Some(PathBuf::from(dir));
                    idx += 1;
                }
            }
            "--minimized" => {
                minimized = true;
            }
            "--tray-locked" => {
                tray_locked = true;
            }
            "--autostart-enable" | "--enable-autostart" => {
                let mut settings = SettingsService::default();
                match settings.set_autostart(true) {
                    Ok(_) => println!("OK :: LuaTools autostart enabled (~/.config/autostart/luatools.desktop)"),
                    Err(e) => {
                        let msg = format!("Failed to enable autostart: {}", e);
                        report_fatal_error("LuaTools Linux — Autostart Error", &msg, true);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "--autostart-disable" | "--disable-autostart" => {
                let mut settings = SettingsService::default();
                match settings.set_autostart(false) {
                    Ok(_) => println!("OK :: LuaTools autostart disabled"),
                    Err(e) => {
                        let msg = format!("Failed to disable autostart: {}", e);
                        report_fatal_error("LuaTools Linux — Autostart Error", &msg, true);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            "-h" | "--help" => {
                println!(
                    "LuaTools Linux v{}\n\n\
                     Usage: luatools [OPTIONS] [luatools://... | bst://...]\n\n\
                     Options:\n  \
                       --check               Verify binary and Steam discovery without opening GUI\n  \
                       --update              Pull latest source from Git and rebuild/install native binary\n  \
                       --minimized           Launch minimized\n  \
                       --tray-locked         Keep running in background/tray on close\n  \
                       --autostart-enable    Enable login autostart (~/.config/autostart/luatools.desktop)\n  \
                       --autostart-disable   Disable login autostart\n  \
                       -v,--version          Print version\n  \
                       -h,--help             Show help",
                    VERSION
                );
                return Ok(());
            }
            other if other.to_ascii_lowercase().starts_with("luatools://")
                || other.to_ascii_lowercase().starts_with("bst://") => {
                initial_protocol_url = Some(other.to_string());
            }
            _ => {}
        }
        idx += 1;
    }

    // LT-01: Single-instance mutex/flock
    // LT-02: Cross-instance IPC forwarding over Unix domain socket
    let (lock_path, sock_path) = get_instance_paths();

    let instance_lock = match InstanceLock::try_acquire(&lock_path) {
        Ok(lock) => lock,
        Err(_) => {
            // Another instance holds the flock lock; forward protocol action, tray-lock, or focus window
            let mut msgs: Vec<String> = Vec::new();
            if tray_locked {
                msgs.push("TRAYLOCK\n".to_string());
            }
            if let Some(url) = &initial_protocol_url {
                msgs.push(format!("URL {}\n", url));
            } else if !minimized {
                msgs.push("FOCUS\n".to_string());
            }

            if msgs.is_empty() {
                return Ok(());
            }

            let payload = msgs.join("");
            let mut connected = false;
            for _ in 0..10 {
                if let Ok(mut stream) = UnixStream::connect(&sock_path) {
                    let _ = stream.write_all(payload.as_bytes());
                    let _ = stream.flush();
                    connected = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            if connected {
                println!("LuaTools is already running. Request forwarded to active instance.");
            } else {
                eprintln!(
                    "LuaTools instance lock held, but could not connect to Unix socket at {}.",
                    sock_path.display()
                );
            }
            return Ok(());
        }
    };

    // LT-10: Self-heal luatools:// and bst:// XDG desktop handlers on startup
    let _ = ProtocolService::register_desktop_handler();

    // We acquired the single-instance lock. Clean up any stale socket and bind listener.
    let _ = std::fs::remove_file(&sock_path);
    let (ipc_tx, ipc_rx) = channel::<IpcCommand>();

    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => {
            let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
            Some(l)
        }
        Err(e) => {
            eprintln!(
                "Warning: failed to bind single-instance IPC socket at {}: {}",
                sock_path.display(),
                e
            );
            None
        }
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("LuaTools  v{}", VERSION))
        .with_app_id("luatools")
        .with_inner_size([1380.0, 860.0])
        .with_min_inner_size([1060.0, 660.0])
        .with_drag_and_drop(true)
        .with_icon(build_app_icon());

    if minimized {
        viewport = viewport.with_visible(false);
    }

    // Prefer X11/XWayland when DISPLAY is available to avoid winit 0.30's wl_seat v10 pointer panic
    if std::env::var_os("WINIT_UNIX_BACKEND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::set_var("WINIT_UNIX_BACKEND", "x11");
    }

    // On hybrid GPU Linux laptops (e.g. AMD/Intel + NVIDIA), libEGL probes every ICD by default
    // and maps both Mesa/LLVM and NVIDIA shader compilers (~40+ MB extra RSS) for a simple 2D UI.
    // Default to Mesa EGL unless the user explicitly requested PRIME offload or another ICD.
    if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_none()
        && std::env::var_os("__NV_PRIME_RENDER_OFFLOAD").is_none()
        && std::env::var_os("__GLX_VENDOR_LIBRARY_NAME").is_none()
    {
        let mesa_icd = Path::new("/usr/share/glvnd/egl_vendor.d/50_mesa.json");
        if mesa_icd.is_file() {
            std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", mesa_icd);
        }
    }

    let native_options = eframe::NativeOptions {
        viewport,
        depth_buffer: 0,
        stencil_buffer: 0,
        multisampling: 0,
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "LuaTools Linux",
        native_options,
        Box::new(move |cc| {
            if let Some(listener) = listener {
                let tx = ipc_tx.clone();
                let repaint_ctx = cc.egui_ctx.clone();
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if let Ok(stream) = stream {
                            let reader = BufReader::new(stream);
                            for line in reader.lines().map_while(Result::ok) {
                                let trimmed = line.trim();
                                if trimmed == "FOCUS" {
                                    let _ = tx.send(IpcCommand::Focus);
                                    repaint_ctx.request_repaint();
                                } else if trimmed == "TRAYLOCK" {
                                    let _ = tx.send(IpcCommand::TrayLock);
                                    repaint_ctx.request_repaint();
                                } else if let Some(url) = trimmed.strip_prefix("URL ") {
                                    let _ = tx.send(IpcCommand::Url(url.trim().to_string()));
                                    repaint_ctx.request_repaint();
                                } else if trimmed.starts_with("luatools://") || trimmed.starts_with("bst://") {
                                    let _ = tx.send(IpcCommand::Url(trimmed.to_string()));
                                    repaint_ctx.request_repaint();
                                }
                            }
                        }
                    }
                });
            }

            let is_silent_init = initial_protocol_url
                .as_ref()
                .and_then(|u| ProtocolService::parse(u))
                .map(|a| a.silent)
                .unwrap_or(false);

            let mut inner = LuaToolsApp::new_with_tray_hint(
                cc,
                initial_protocol_url,
                qa_screenshots_dir,
                minimized || tray_locked || is_silent_init,
            );
            inner.tray_locked = tray_locked;

            Ok(Box::new(AppWrapper {
                inner,
                ipc_rx,
                tray_locked,
                initial_minimized: minimized || is_silent_init,
                sock_path,
                _instance_lock: instance_lock,
            }))
        }),
    ) {
        let exe_note = main_exe_path
            .as_ref()
            .map(|p| format!(" ({})", p.display()))
            .unwrap_or_default();
        let msg = format!("LuaTools Linux{} failed to start GUI: {}", exe_note, err);
        report_fatal_error("LuaTools Linux — Startup Error", &msg, true);
        return Err(err);
    }

    Ok(())
}

fn build_app_icon() -> egui::IconData {
    let w = 64usize;
    let h = 64usize;
    let mut rgba = vec![0u8; w * h * 4];

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let mut color = [12u8, 13, 16, 255]; // #0C0D10 monotone void
            if (3..61).contains(&x) && (3..61).contains(&y) {
                color = [24, 26, 32, 255]; // #181A20 paper card
            }
            if (3..8).contains(&x) && (3..61).contains(&y) {
                color = [26, 58, 107, 255]; // #1A3A6B deep sapphire
            }
            if ((16..23).contains(&x) && (17..48).contains(&y))
                || ((16..34).contains(&x) && (41..48).contains(&y))
            {
                color = [239, 240, 244, 255];
            }
            if ((32..53).contains(&x) && (17..24).contains(&y))
                || ((39..46).contains(&x) && (17..48).contains(&y))
            {
                color = [118, 162, 230, 255];
            }
            rgba[idx..idx + 4].copy_from_slice(&color);
        }
    }

    egui::IconData {
        rgba,
        width: w as u32,
        height: h as u32,
    }
}
