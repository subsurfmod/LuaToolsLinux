//! Entry point for LuaTools Linux v2.0.0 native desktop application.

use eframe::egui;
use luatools_linux::services::settings_service::SettingsService;
use luatools_linux::services::steam_service::detect_steam_install;
use luatools_linux::services::update_service::UpdateService;
use luatools_linux::ui::app::LuaToolsApp;
use luatools_linux::VERSION;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_protocol_url: Option<String> = None;

    for arg in &args {
        match arg.as_str() {
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
    }

    let viewport = egui::ViewportBuilder::default()
        .with_title(format!("LUATOOLS // LINUX  v{}", VERSION))
        .with_app_id("luatools")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([1060.0, 660.0])
        .with_drag_and_drop(true)
        .with_icon(build_app_icon());

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "LuaTools Linux",
        native_options,
        Box::new(move |cc| Ok(Box::new(LuaToolsApp::new(cc, initial_protocol_url)))),
    )
}

fn build_app_icon() -> egui::IconData {
    let w = 64usize;
    let h = 64usize;
    let mut rgba = vec![0u8; w * h * 4];

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let mut color = [9u8, 9, 11, 255]; // #09090B void
            if (2..62).contains(&x) && (2..62).contains(&y) {
                color = [20, 20, 24, 255]; // #141418 panel
            }
            if (2..6).contains(&x) && (2..62).contains(&y) {
                color = [37, 99, 235, 255]; // #2563EB left bar
            }
            if (6..62).contains(&x) && (2..4).contains(&y) {
                color = [59, 130, 246, 255]; // #3B82F6 top edge
            }
            // Crisp 'L' glyph
            if ((14..21).contains(&x) && (17..48).contains(&y))
                || ((14..33).contains(&x) && (41..48).contains(&y))
            {
                color = [244, 244, 246, 255];
            }
            // Crisp 'T' glyph in cobalt blue
            if ((31..53).contains(&x) && (17..24).contains(&y))
                || ((39..46).contains(&x) && (17..48).contains(&y))
            {
                color = [59, 130, 246, 255];
            }
            // Accent corner square
            if (48..54).contains(&x) && (42..48).contains(&y) {
                color = [96, 165, 250, 255];
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
