//! Entry point for LuaTools Linux v2.0-beta native desktop application.

use std::path::PathBuf;

use eframe::egui;
use luatools_linux::services::settings_service::SettingsService;
use luatools_linux::services::steam_service::detect_steam_install;
use luatools_linux::services::update_service::UpdateService;
use luatools_linux::ui::app::LuaToolsApp;
use luatools_linux::VERSION;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_protocol_url: Option<String> = None;
    let mut qa_screenshots_dir: Option<PathBuf> = None;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "-v" | "--version" => {
                println!("luatools-linux v{}", VERSION);
                return Ok(());
            }
            "--check" => {
                let steam = detect_steam_install(None);
                match steam {
                    Some(inst) => println!(
                        "OK :: LuaTools Linux v{} · Steam={} ({} libraries)",
                        VERSION,
                        inst.root.display(),
                        inst.libraries.len()
                    ),
                    None => println!(
                        "OK :: LuaTools Linux v{} · Steam=not detected (standalone ready)",
                        VERSION
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
                        eprintln!("ERROR :: {}", e);
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
            "-h" | "--help" => {
                println!(
                    "LuaTools Linux v{}\n\nUsage: luatools [OPTIONS] [luatools://...]\n\nOptions:\n  --check      Verify binary and Steam discovery without opening GUI\n  --update     Pull latest source from Git and rebuild/install native binary\n  -v,--version Print version\n  -h,--help    Show help",
                    VERSION
                );
                return Ok(());
            }
            other if other.to_ascii_lowercase().starts_with("luatools://") => {
                initial_protocol_url = Some(other.to_string());
            }
            _ => {}
        }
        idx += 1;
    }

    let viewport = egui::ViewportBuilder::default()
        .with_title(format!("LuaTools  v{}", VERSION))
        .with_app_id("luatools")
        .with_inner_size([1380.0, 860.0])
        .with_min_inner_size([1060.0, 660.0])
        .with_drag_and_drop(true)
        .with_icon(build_app_icon());

    // Prefer X11/XWayland when DISPLAY is available to avoid winit 0.30's wl_seat v10 pointer panic
    if std::env::var_os("WINIT_UNIX_BACKEND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::set_var("WINIT_UNIX_BACKEND", "x11");
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "LuaTools Linux",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(LuaToolsApp::new(
                cc,
                initial_protocol_url,
                qa_screenshots_dir,
            )))
        }),
    )
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
