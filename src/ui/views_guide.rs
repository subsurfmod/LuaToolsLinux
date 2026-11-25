//! Illustrated "How to Use" Manual (`ActiveView::Guide`) — rewritten interactive visual guide
//! with embedded application screenshots, click-to-enlarge preview modal, Quick Reference task table,
//! syntax-highlighted `.lua` reference, and one-click navigation into every workspace.

use eframe::egui::{
    self, Align, ColorImage, CornerRadius, CursorIcon, FontFamily, FontId, Layout, Margin,
    RichText, ScrollArea, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, Vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::ui::app::LuaToolsApp;
use crate::ui::state::{ActiveView, DepotsSubTab, GuideTopic, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    chip_button, display_font, ghost_button, inset_paper_sheet, lua_layouter, paper_card,
    paper_card_padded, primary_button, secondary_button, section_header, semibold_font,
    status_badge, BadgeTone, Palette,
};

const SHOT_HOME: &[u8] = include_bytes!("../../assets/guide/01_home_overview.png");
const SHOT_DISCOVER: &[u8] = include_bytes!("../../assets/guide/02_discover_store.png");
const SHOT_DEPOTS: &[u8] = include_bytes!("../../assets/guide/03_depots_vault_binding.png");
const SHOT_INTEGRATION: &[u8] =
    include_bytes!("../../assets/guide/04_library_integration_modal.png");
const SHOT_STEAM_BRIDGE: &[u8] =
    include_bytes!("../../assets/guide/05_settings_steam_integration.png");
const SHOT_WIRED_CLOUD: &[u8] = include_bytes!("../../assets/guide/06_settings_wired_cloud.png");

fn guide_screenshot_bytes(key: &str) -> Option<&'static [u8]> {
    match key {
        "01_home_overview" => Some(SHOT_HOME),
        "02_discover_store" => Some(SHOT_DISCOVER),
        "03_depots_vault_binding" => Some(SHOT_DEPOTS),
        "04_library_integration_modal" => Some(SHOT_INTEGRATION),
        "05_settings_steam_integration" => Some(SHOT_STEAM_BRIDGE),
        "06_settings_wired_cloud" => Some(SHOT_WIRED_CLOUD),
        _ => None,
    }
}

impl LuaToolsApp {
    pub fn ensure_guide_texture(
        &mut self,
        ctx: &egui::Context,
        key: &'static str,
    ) -> Option<TextureHandle> {
        if let Some(existing) = self.guide_textures.get(key) {
            return Some(existing.clone());
        }
        let raw = guide_screenshot_bytes(key)?;
        let img = image::load_from_memory(raw).ok()?;
        let resized = if img.width() > 760 {
            let new_h = ((img.height() as f32) * (760.0 / img.width() as f32)).round() as u32;
            img.resize(760, new_h.max(1), image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let decoded = resized.into_rgba8();
        let (w, h) = decoded.dimensions();
        let color_img =
            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], decoded.as_raw());
        let handle = ctx.load_texture(
            format!("guide_shot_{}", key),
            color_img,
            TextureOptions::LINEAR,
        );
        self.guide_textures.insert(key, handle.clone());
        Some(handle)
    }

    pub fn render_guide_zoom_modal(&mut self, ctx: &egui::Context) {
        let Some((key, title)) = self.guide_zoom_screenshot else {
            return;
        };
        let tex_opt = self.ensure_guide_texture(ctx, key);
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let screen = ctx.screen_rect();
        let max_w = (screen.width() - 96.0).clamp(520.0, 1220.0);
        let max_h = (screen.height() - 150.0).clamp(320.0, 780.0);

        egui::Window::new(format!("App Preview — {}", title))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if let Some(tex) = tex_opt {
                    let native = tex.size_vec2();
                    let aspect = (native.x / native.y.max(1.0)).max(0.5);
                    let mut draw_w = max_w;
                    let mut draw_h = draw_w / aspect;
                    if draw_h > max_h {
                        draw_h = max_h;
                        draw_w = draw_h * aspect;
                    }
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(draw_w, draw_h), Sense::click());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    ui.painter().image(
                        tex.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(4),
                        Stroke::new(1.0_f32, Palette::BORDER_STRONG),
                        egui::StrokeKind::Inside,
                    );
                    if resp.clicked() {
                        close = true;
                    }
                } else {
                    ui.label(
                        RichText::new("Preview image unavailable.")
                            .size(13.0)
                            .color(Palette::TEXT_MUTED),
                    );
                }
                ui.add_space(8.0);
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.label(
                            RichText::new("Click image or press Esc to close preview")
                                .size(12.0)
                                .color(Palette::TEXT_MUTED),
                        );
                    },
                    |ui| {
                        if primary_button(ui, "Close Preview").clicked() {
                            close = true;
                        }
                    },
                );
            });

        if close {
            self.guide_zoom_screenshot = None;
        }
    }

    pub fn render_guide(&mut self, ui: &mut egui::Ui) {
        let mut open_bug_report = false;
        let mut open_telemetry_settings = false;

        section_header(
            ui,
            "MANUAL",
            "How to Use LuaTools Linux",
            "Illustrated step-by-step guide with app screenshots, one-click game setup, Steam Client Interop, LuaVault, and fixes",
            |ui| {
                if ghost_button(ui, "Telemetry Settings").clicked() {
                    open_telemetry_settings = true;
                }
                if secondary_button(ui, "Report a Bug").clicked() {
                    open_bug_report = true;
                }
            },
        );

        if open_telemetry_settings {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Telemetry;
            return;
        }
        if open_bug_report {
            match self.telemetry_service.open_github_bug_report() {
                Ok(m) => self.notify(m, ToastKind::Info),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }

        // Topic Navigation Chips
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let topics = [
                ("1. Quick Start", GuideTopic::QuickStart),
                ("2. Adding Games & .lua", GuideTopic::AddingGames),
                ("3. Depots, Vault & Binding", GuideTopic::DepotsAndVault),
                (
                    "4. One-Click Fixes & Library",
                    GuideTopic::FixesAndOverrides,
                ),
                ("5. Steam Interop, Wired & Cloud", GuideTopic::SteamInterop),
                (
                    "6. FAQ, Shortcuts & Bug Reports",
                    GuideTopic::Troubleshooting,
                ),
            ];
            for (lbl, topic) in topics {
                if chip_button(ui, lbl, self.guide_topic == topic).clicked() {
                    self.guide_topic = topic;
                }
            }
        });

        ui.add_space(16.0);

        ScrollArea::vertical()
            .id_salt(("how_to_use_scroll", self.guide_topic as usize))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(940.0);
                match self.guide_topic {
                    GuideTopic::QuickStart => self.render_guide_quick_start(ui),
                    GuideTopic::AddingGames => self.render_guide_adding_games(ui),
                    GuideTopic::DepotsAndVault => self.render_guide_depots_and_vault(ui),
                    GuideTopic::FixesAndOverrides => self.render_guide_fixes_and_overrides(ui),
                    GuideTopic::SteamInterop => self.render_guide_steam_interop(ui),
                    GuideTopic::Troubleshooting => self.render_guide_troubleshooting(ui),
                }
                ui.add_space(24.0);
            });
    }

    fn render_guide_screenshot_card(
        &mut self,
        ui: &mut egui::Ui,
        key: &'static str,
        title: &'static str,
        caption: &str,
    ) {
        let tex_opt = self.ensure_guide_texture(ui.ctx(), key);
        let mut clicked_zoom = false;

        paper_card_padded(ui, Margin::same(14), |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;

            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        status_badge(ui, "SCREENSHOT", BadgeTone::Blue);
                        ui.label(
                            RichText::new(title)
                                .font(semibold_font(13.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                    });
                },
                |ui| {
                    if ghost_button(ui, "Enlarge Preview").clicked() {
                        clicked_zoom = true;
                    }
                },
            );

            if let Some(tex) = tex_opt {
                let avail_w = ui.available_width().clamp(320.0, 900.0);
                let native = tex.size_vec2();
                let aspect = (native.x / native.y.max(1.0)).max(0.5);
                let draw_h = (avail_w / aspect).round();

                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(avail_w, draw_h), Sense::click());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                let border_col = if resp.hovered() {
                    Palette::BLUE_LIGHT
                } else {
                    Palette::BORDER_DEFAULT
                };
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(4),
                    Stroke::new(1.0_f32, border_col),
                    egui::StrokeKind::Inside,
                );
                if resp
                    .on_hover_text("Click screenshot to open full-size preview")
                    .clicked()
                {
                    clicked_zoom = true;
                }
            }

            ui.label(RichText::new(caption).size(12.0).color(Palette::TEXT_MUTED));
        });

        if clicked_zoom {
            self.guide_zoom_screenshot = Some((key, title));
        }
    }

    fn render_guide_quick_start(&mut self, ui: &mut egui::Ui) {
        let mut nav_target: Option<(ActiveView, Option<SettingsSubTab>)> = None;
        let mut open_create_modal = false;

        guide_step_card(
            ui,
            "STEP 01",
            "Set Up Steam Unlocker (SLSsteam) or Rootless Bridge",
            "- Click the top-right 'Setup Steam' pill (or open Settings › Steam Setup and click 'Install Steam Unlocker (SLSsteam)').\n\
             - Optional: Open Settings › Advanced Bridge and click 'Install Integration (No Root)' to stage ~/.local/lib/luatools/libsteam_integration.so and ~/.local/bin/luatools-steam-launcher — zero root or sudo required.",
            |ui| {
                if primary_button(ui, "Steam Setup ›").clicked() {
                    nav_target = Some((ActiveView::Settings, Some(SettingsSubTab::Integrations)));
                }
                if secondary_button(ui, "Advanced Bridge ›").clicked() {
                    nav_target = Some((
                        ActiveView::Settings,
                        Some(SettingsSubTab::SteamIntegration),
                    ));
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "STEP 02",
            "Add a Game Manifest & Register in Your Steam Library",
            "- Drag and drop any .zip, .lua, or .manifest package onto the Home Drop Sheet (or click 'Browse Files…').\n\
             - Or open Add Games (Ctrl+2), search any Steam title, sign in with Discord or a Hubcap key, and click 'Install Game'.\n\
             - Click '+ Add to Steam Library' in Add Games (or 'My Games › More › Add Steam Library Entry') so Lua-only titles appear directly in Steam.",
            |ui| {
                if primary_button(ui, "Open Add Games ›").clicked() {
                    nav_target = Some((ActiveView::Acquire, None));
                }
                if secondary_button(ui, "+ Add by AppID").clicked() {
                    open_create_modal = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "STEP 03",
            "Open Integration & Quick Setup, Then Launch Steam",
            "- Click 'Open' on any added game card on Home, 'Integrate' on any row in My Games (Ctrl+3), or 'Open Setup / Integration' in Add Games to open the Integration & Process Controls window.\n\
             - Click 'Apply Recommended Fixes Now' to enable Play-Unowned, set AppID 480 redirect, and apply Goldberg in one click, then click the top-right 'Launch Steam' (or 'Restart Steam') button.",
            |ui| {
                if primary_button(ui, "Open My Games ›").clicked() {
                    nav_target = Some((ActiveView::Library, None));
                }
                if ghost_button(ui, "Settings › General").clicked() {
                    nav_target = Some((ActiveView::Settings, Some(SettingsSubTab::General)));
                }
            },
        );

        ui.add_space(14.0);

        self.render_guide_screenshot_card(
            ui,
            "01_home_overview",
            "Home Workspace & Recently Added Games",
            "Drop .lua/.manifest/.zip packages onto the main sheet, click 'Open' on any recently added game to launch Integration & Process Controls, or click 'Depots & DLC' to manage files.",
        );

        ui.add_space(14.0);

        // Quick Reference Grid for Common Tasks
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Quick Reference — Common Tasks")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.add_space(8.0);

            let tasks = [
                (
                    "1-click SLSsteam setup",
                    "Settings › Steam Setup › Install Steam Unlocker (SLSsteam)",
                ),
                (
                    "Open per-game Integration window",
                    "Home › Open (or My Games › Integrate / Add Games › Open Setup / Integration)",
                ),
                (
                    "Apply 1-click recommended fixes",
                    "Integration window › Apply Recommended Fixes Now",
                ),
                (
                    "Start / restart / stop Steam",
                    "Top-right bar: Launch Steam / Restart Steam / Stop Steam",
                ),
                (
                    "Edit a game's .lua & FakeAppId",
                    "My Games › Edit .lua (or Integration window › Edit .lua)",
                ),
                (
                    "Make unowned game appear in Steam",
                    "Close Steam, then Add Games › + Add to Steam Library (or My Games › More › Add Steam Library Entry)",
                ),
                (
                    "Lock or switch a game patch/build",
                    "Depots & Vault › Manifests & Vault (LuaVault) or Manifest Binding",
                ),
                (
                    "Download depots directly",
                    "Depots & Vault › Download Depots (supports QR sign-in & -depotkeys)",
                ),
                (
                    "Install Goldberg / Steamless",
                    "Settings › Updates & Tools › Install / Update",
                ),
                (
                    "Sync saves with Cloud Redirect",
                    "Settings › Steam Setup › Cloud Save Redirect › Sync to CloudRedirect",
                ),
            ];

            inset_paper_sheet(ui, Margin::same(10), |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(250.0))
                    .column(Column::remainder())
                    .header(26.0, |mut h| {
                        for col in ["Goal", "Where in LuaTools Linux"] {
                            h.col(|ui| {
                                ui.label(
                                    RichText::new(col)
                                        .font(semibold_font(12.0))
                                        .color(Palette::TEXT_MUTED),
                                );
                            });
                        }
                    })
                    .body(|mut body| {
                        for (goal, path) in tasks {
                            body.row(30.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(goal)
                                            .font(semibold_font(12.5))
                                            .color(Palette::TEXT_PRIMARY),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(path)
                                            .font(FontId::new(12.0, FontFamily::Monospace))
                                            .color(Palette::BLUE_LIGHT),
                                    );
                                });
                            });
                        }
                    });
            });
        });

        ui.add_space(14.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("Local File Architecture on Linux")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            guide_bullet(
                ui,
                "~/.local/share/Steam/config/stplug-in/<appid>.lua  (+ play_unowned_games.lua)",
                "Base AppID, DLC entitlements, 64-char hex AES-256 depot keys, pinned manifest GIDs, and multi-app PlayUnowned table.",
            );
            guide_bullet(
                ui,
                "~/.local/share/Steam/depotcache/<depot>_<gid>.manifest",
                "Binary Steam depot manifest snapshots extracted from .zip packages for offline or build-locked installs.",
            );
            guide_bullet(
                ui,
                "~/.local/share/luatools/luavault/<appid>/  &  manifest_pins.json",
                "Content-addressed SHA-256 build snapshots for instant version switching plus persistent Depot Manifest Binding pins.",
            );
            guide_bullet(
                ui,
                "~/.local/lib/luatools/libsteam_integration.so  &  ~/.local/bin/luatools-steam-launcher",
                "Rootless LD_PRELOAD Steam Client Interop library & Unix IPC server (auto-loads stplug-in/*.lua, bridge_overrides.json, and tickets).",
            );
            guide_bullet(
                ui,
                "~/.config/SLSsteam/config.yaml  &  ~/.local/share/luatools/goldberg/",
                "SLSsteam YAML configuration (AdditionalApps, FakeAppIds, DepotKeys) and staged native Linux Goldberg emulator libraries.",
            );
        });

        if open_create_modal {
            self.add_appid_modal.open = true;
        }
        if let Some((view, sub)) = nav_target {
            self.active_view = view;
            if let Some(s) = sub {
                self.settings_subtab = s;
            }
        }
    }

    fn render_guide_adding_games(&mut self, ui: &mut egui::Ui) {
        let mut nav_discover = false;
        let mut nav_library = false;
        let mut nav_account = false;
        let mut focus_omnibox = false;

        self.render_guide_screenshot_card(
            ui,
            "02_discover_store",
            "Add Games — Cloud Providers, Setup Shortcuts & Library Registration",
            "Browse Top Sellers, New Releases, or search any Steam title to check live manifest providers (Morrenus, Hubcap), click 'Install Game', generate DLC .lua files, or jump straight to 'Open Setup / Integration', 'Files & DLC', 'Game Fixes', and 'Launch Options'.",
        );

        ui.add_space(14.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("Four Ways to Add Games & Manifests")
                    .font(display_font(18.5))
                    .color(Palette::TEXT_PRIMARY),
            );

            guide_bullet(
                ui,
                "1. Drag & Drop / File Picker (Offline Packages)",
                "Drop <appid>.zip, <appid>.lua, <appid>_<buildid>.lua, or <depot>_<gid>.manifest anywhere on LuaTools (or click 'Browse Files…' on Home). \
                 Archives are verified against ZipSlip path traversal: .lua files install to stplug-in/, .manifest files copy \
                 to depotcache/, a SHA-256 snapshot is stored in LuaVault, and SLSsteam config.yaml updates automatically.",
            );
            guide_bullet(
                ui,
                "2. Cloud Manifest Providers (Add Games)",
                "Sign in with Discord under Settings › Account (browser OAuth or 6-character /login bot code), \
                 or paste a Hubcap API key (smm_ + 96 hex characters). Select any game in Add Games and click 'Install Game' \
                 (with 'FastFetch' auto-selecting the first available provider).",
            );
            guide_bullet(
                ui,
                "3. 1-Click Steam Library Entry (ACF)",
                "Click '+ Add to Steam Library' in Add Games or 'My Games › More › Add Steam Library Entry'. LuaTools writes \
                 a valid Valve \"AppState\" appmanifest_<appid>.acf, creates <steamapps>/common/<installdir>, updates libraryfolders.vdf, \
                 and sets FakeAppId 480 in SLSsteam. Close Steam before writing .acf files so Steam does not overwrite them on exit.",
            );
            guide_bullet(
                ui,
                "4. Global Quick Search & Command Bar (Ctrl+K / Ctrl+L)",
                "Press Ctrl+K from any tab and paste a numeric AppID (e.g. 1245620), a Steam Store URL, a SteamDB URL, \
                 a steam:// link, a luatools://install/<appid> URI, a local file path, or a game title to search.",
            );

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if primary_button(ui, "Open Add Games").clicked() {
                    nav_discover = true;
                }
                if secondary_button(ui, "Open My Games").clicked() {
                    nav_library = true;
                }
                if secondary_button(ui, "Account & Hubcap Sign-In").clicked() {
                    nav_account = true;
                }
                if ghost_button(ui, "Focus Search Bar (Ctrl+K)").clicked() {
                    focus_omnibox = true;
                }
            });
        });

        if nav_discover {
            self.active_view = ActiveView::Acquire;
        }
        if nav_library {
            self.active_view = ActiveView::Library;
        }
        if nav_account {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Account;
        }
        if focus_omnibox {
            self.focus_command_next = true;
        }

        ui.add_space(14.0);

        let mut copy_sample: Option<String> = None;
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            let mut sample = String::from(
                "-- 1. Base game entitlement\n\
                 addappid(1245620) -- ELDEN RING\n\n\
                 -- 2. Encrypted content depot (64-char hex AES-256 depot key -> DepotKeyIntegration)\n\
                 addappid(1245621, 1, \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\") -- Core Game\n\
                 setManifestid(1245621, \"8492018394810293847\", 52428800000) -- Active pin locks this exact build\n\
                 -- setManifestid(1245622, \"1111222233334444555\", 2048) -- Commented pin allows Steam auto-updates\n\n\
                 -- 3. DLC entitlement (no key needed when content is shared in base depot)\n\
                 addappid(2778580) -- Shadow of the Erdtree\n\n\
                 -- 4. Optional app ownership token (synced to SLSsteam AppTokens)\n\
                 addtoken(1245620, \"YOUR_OPTIONAL_APP_TOKEN\")",
            );
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Annotated stplug-in .lua Format Reference")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if ghost_button(ui, "Copy Example .lua").clicked() {
                        copy_sample = Some(sample.clone());
                    }
                },
            );
            ui.label(
                RichText::new(
                    "Understanding each directive inside ~/.local/share/Steam/config/stplug-in/<appid>.lua:",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );

            let mut layouter =
                |ui: &egui::Ui, string: &str, wrap_width: f32| lua_layouter(ui, string, wrap_width);
            inset_paper_sheet(ui, Margin::same(12), |ui| {
                ui.add(
                    TextEdit::multiline(&mut sample)
                        .code_editor()
                        .interactive(false)
                        .desired_rows(11)
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .layouter(&mut layouter),
                );
            });
        });
        if let Some(txt) = copy_sample {
            ui.ctx().copy_text(txt);
            self.notify(
                "Copied example .lua template to clipboard.",
                ToastKind::Info,
            );
        }
    }

    fn render_guide_depots_and_vault(&mut self, ui: &mut egui::Ui) {
        let mut go_depots: Option<DepotsSubTab> = None;

        self.render_guide_screenshot_card(
            ui,
            "03_depots_vault_binding",
            "Depots & LuaVault Workbench",
            "Switch stored SHA-256 .lua presets in LuaVault on the left, inspect depots and toggle Pin/Unpin status on the right, or switch tabs to Lua Editor, Download Depots, and Manifest Binding.",
        );

        ui.add_space(14.0);

        guide_step_card(
            ui,
            "LUAVAULT",
            "Switching Game Builds & Saving Presets",
            "Every installed .lua or <appid>_<buildid>.lua file is snapshotted by SHA-256 under ~/.local/share/luatools/luavault/<appid>/.\n\
             - Select any stored build in Depots & Vault › Manifests & Vault and click 'Apply Selected Variant' (or right-click to Apply, Rename, or Delete).\n\
             - Click '+ Save Preset' before editing or downgrading a game build so you can roll back in one click.",
            |ui| {
                if secondary_button(ui, "Open Manifests & Vault ›").clicked() {
                    go_depots = Some(DepotsSubTab::Declarations);
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "PINNING",
            "Pinned Builds vs Auto-Updating Games & Live Lua Editor",
            "- Pinned (setManifestid active): Locks a depot to a specific manifest GID — ideal when mods or fixes require an exact patch.\n\
             - Auto-Update (-- setManifestid commented out): Allows Steam to download the newest public build.\n\
             - Click 'Pin' / 'Unpin' on any row in Depot Declarations, or open 'Lua Editor' to edit stplug-in/<appid>.lua directly with syntax highlighting.",
            |ui| {
                if ghost_button(ui, "Open Live Lua Editor ›").clicked() {
                    go_depots = Some(DepotsSubTab::LiveEditor);
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "BINDING",
            "Depot Manifest Binding (IPC Enforcement)",
            "In Depots & Vault › Manifest Binding, LuaTools synchronizes per-depot manifest locks with the Steam Client Interop bridge:\n\
             - Active setManifestid(...) entries import automatically, or you can enter a Depot ID + 19-digit Manifest GID and click '+ Add Manifest Pin'.\n\
             - Pins persist in ~/.local/share/luatools/manifest_pins.json and show 'Active (Bound)' when confirmed over Unix IPC by libsteam_integration.so, or 'Pending (Saved)' until Steam launches with the bridge.",
            |ui| {
                if secondary_button(ui, "Open Manifest Binding ›").clicked() {
                    go_depots = Some(DepotsSubTab::ManifestBinding);
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "DOWNLOADER",
            "Downloading Depots via DepotDownloader / DepotDownloaderMod",
            "In Depots & Vault › Download Depots:\n\
             1. Pick an Output Folder and tick the content depots you want (shared redistributables and keyless DLCs are filtered out automatically).\n\
             2. For non-free depots on official SteamRE/DepotDownloader, click 'Sign In (QR)' once to cache an authenticated session with -remember-password.\n\
             3. Click 'Download Selected' — LuaTools passes all known .lua and SLSsteam 64-char hex keys via a temporary 0600 -depotkeys file and deletes it immediately afterward.",
            |ui| {
                if primary_button(ui, "Open Depot Downloader ›").clicked() {
                    go_depots = Some(DepotsSubTab::DownloadPicker);
                }
            },
        );

        if let Some(sub) = go_depots {
            self.active_view = ActiveView::Depots;
            self.depots_subtab = sub;
            if sub == DepotsSubTab::DownloadPicker {
                self.rebuild_depot_picks();
            }
        }
    }

    fn render_guide_fixes_and_overrides(&mut self, ui: &mut egui::Ui) {
        let mut go_library = false;
        let mut go_fixes = false;
        let mut go_tools = false;

        self.render_guide_screenshot_card(
            ui,
            "04_library_integration_modal",
            "One-Click Game Setup — Integration & Process Controls Modal",
            "Click 'Open' on any added game on Home (or 'Integrate' in My Games) and click 'Apply Recommended Fixes Now' to enable Play-Unowned, set the 480 AppID redirect, and apply native Goldberg in one click — plus jump straight to Files & DLC, Edit .lua, Launch Options, or Game Fixes from the top Game Tools bar.",
        );

        ui.add_space(14.0);

        guide_step_card(
            ui,
            "QUICK SETUP",
            "How to Open Integration & Process Controls",
            "Open the Integration & Process Controls window in 1 click from anywhere:\n\
             - Home: Click 'Open' (or click the cover image) on any card under Recently Added Games.\n\
             - My Games (Ctrl+3): Click 'Integrate' right on any game row (or click the game's title).\n\
             - Add Games / Depots & Vault / Game Fixes: Click 'Open Setup / Integration' or 'Integrate / Setup'.\n\
             Inside, click 'Apply Recommended Fixes Now', configure CD/depot keys or Steam stats, or use the top 'Game Tools' bar to switch straight to Files & DLC, Edit .lua, Launch Options, or Game Fixes.",
            |ui| {
                if primary_button(ui, "Open My Games ›").clicked() {
                    go_library = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "MY GAMES & VDF",
            "Direct Row Actions & Flattened More Menu",
            "Each row in My Games gives direct 1-click buttons plus a flat, clearly labeled More menu:\n\
             - Integrate: Opens Integration & Process Controls (Recommended Fixes, AppID 480 redirect, keys, LD_PRELOAD).\n\
             - Files & DLC: Opens Depots & Vault to toggle DLCs, switch saved .lua builds, pin manifests, or download depots.\n\
             - Edit .lua: Edits SLSsteam FakeAppId (480), AppToken, and live stplug-in/<appid>.lua.\n\
             - More: Flat menu with Game Fixes…, Launch Options…, Launch Game with Bridge, Add / Remove Steam Library Entry, Enable / Disable Play Unowned, Offline Patch (Goldberg), Remove SteamStub DRM (Steamless), Clean Old Unlockers, Open Game Folder, and Remove from LuaTools.",
            |ui| {
                if secondary_button(ui, "Browse My Games ›").clicked() {
                    go_library = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "GAME FIXES",
            "Verified Game Fixes with SHA-256 Backup & Clean Revert",
            "In Game Fixes (Ctrl+5), filter by tag or 'My Games only':\n\
             - Install Manifest: Installs the exact build-pinned manifest required by the fix.\n\
             - Apply Fix: Hashes existing game files with SHA-256 and backs them up into <GameDir>/.luatools-fix/backups/<fix_id>/ before extracting.\n\
             - Revert: Verifies post-apply SHA-256 hashes, removes newly added fix files, and restores every original binary byte-for-byte.",
            |ui| {
                if primary_button(ui, "Browse Game Fixes ›").clicked() {
                    go_fixes = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "TOOLCHAIN",
            "Native Goldberg Emulator, SteamAutoPatch, StubRemover & Git Self-Updater",
            "Under Settings › Updates & Tools:\n\
             - Goldberg Emulator (Native Linux): Installs Detanup01/gbe_fork Linux/Proton binaries (libsteam_api.so / steam_api64.dll) into ~/.local/share/luatools/goldberg/ so native SteamAutoPatch works offline.\n\
             - DepotDownloaderMod / SteamAutoPatch / StubRemover (Steamless): One-click GitHub release installers with plain-English explanations.\n\
             - Git Self-Updater: Rebuilds LuaTools and atomically swaps the executable.",
            |ui| {
                if secondary_button(ui, "Open Updates & Tools ›").clicked() {
                    go_tools = true;
                }
            },
        );

        if go_library {
            self.active_view = ActiveView::Library;
        }
        if go_fixes {
            self.active_view = ActiveView::Fixes;
        }
        if go_tools {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::UpdatesAndTools;
        }
    }

    fn render_guide_steam_interop(&mut self, ui: &mut egui::Ui) {
        let mut go_settings: Option<SettingsSubTab> = None;

        self.render_guide_screenshot_card(
            ui,
            "05_settings_steam_integration",
            "Settings › Advanced Bridge — Rootless LD_PRELOAD, ELF64 Patterns & Hot-Reload",
            "Install the rootless launcher (~/.local/bin/luatools-steam-launcher), inspect SHA-256 pattern matches on steamclient.so and steamui.so, and monitor real-time inotify events in stplug-in/.",
        );

        ui.add_space(14.0);

        guide_step_card(
            ui,
            "ADVANCED BRIDGE",
            "Steam Inlibrary Fix (libsteam_integration.so) & Live Hot-Reload",
            "Settings › Advanced Bridge controls the optional rootless Steam Client Interop layer:\n\
             - Install Integration (No Root): Stages libsteam_integration.so and creates ~/.local/bin/luatools-steam-launcher (system-wide /etc/ld.so.preload is optional and not needed).\n\
             - Steam Inlibrary Fix Hooks: Hooks BIsSubscribedApp, GetAppId (480 spoof), GetAppOwnershipTicket, RequestEncryptedAppTicket / GetEncryptedAppTicket, GetSteamID, and DepotKeyIntegration.\n\
             - Automatic Startup Sync: On Steam startup, libsteam_integration.so automatically loads stplug-in/*.lua, bridge_overrides.json, manifest_pins.json, and localconfig.vdf tickets — plus reloads automatically whenever the inotify Hot-Reload Watcher detects .lua edits.",
            |ui| {
                if primary_button(ui, "Open Advanced Bridge ›").clicked() {
                    go_settings = Some(SettingsSubTab::SteamIntegration);
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "STEAM SETUP & CLOUD",
            "1-Click SLSsteam, Cloud Save Redirect & Store Page Plugin",
            "In Settings › Steam Setup, every integration sits on its own open card:\n\
             - 1-Click Steam Unlocker (SLSsteam): Installs SLSsteam (and optional ACCELA) natively in one click without root.\n\
             - Cloud Save Redirect: Restores per-game save cloud sync — tick games or enter an AppID and click 'Sync to CloudRedirect'.\n\
             - Steam Store Page Plugin & URI Links: Click 'Install Store Page Plugin', 'Register luatools:// Links', 'Enable Steam CEF Debugging', or 'Check Millennium Status'.",
            |ui| {
                if secondary_button(ui, "Open Steam Setup ›").clicked() {
                    go_settings = Some(SettingsSubTab::Integrations);
                }
            },
        );

        ui.add_space(14.0);

        self.render_guide_screenshot_card(
            ui,
            "06_settings_wired_cloud",
            "Settings › Steam Setup — SLSsteam, Cloud Save Redirect & Store Plugin",
            "Install SLSsteam in one click, toggle PlayNotOwnedGames, configure Cloud Save Redirect, and install the Steam Store Page Plugin.",
        );

        if let Some(sub) = go_settings {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = sub;
        }
    }

    fn render_guide_troubleshooting(&mut self, ui: &mut egui::Ui) {
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Common Troubleshooting & Solutions")
                    .font(display_font(18.5))
                    .color(Palette::TEXT_PRIMARY),
            );

            guide_bullet(
                ui,
                "Game does not appear in Steam after installing a .lua file",
                "1) Click 'Open' on the game card on Home (or 'Integrate' in My Games) and click 'Apply Recommended Fixes Now'. \
                 2) For Lua-only titles without local files yet, close Steam ('Stop Steam') and click 'My Games › More › Add Steam Library Entry'. \
                 3) Click 'Launch Steam' (or 'Restart Steam') in the top-right bar.",
            );
            guide_bullet(
                ui,
                "Manifest Binding pin shows 'Pending (Saved)' instead of 'Active (Bound)'",
                "'Pending (Saved)' means the pin is stored in manifest_pins.json, waiting for Steam to run with libsteam_integration.so. \
                 Click 'Restart Steam' in the top-right bar, then click 'Re-Sync Pins to Bridge' in Depots & Vault › Manifest Binding.",
            );
            guide_bullet(
                ui,
                "SteamAutoPatch says Goldberg Emulator is not installed",
                "Open Settings › Updates & Tools and click 'Install / Update' on 'Goldberg Emulator (Native Linux)' to stage libsteam_api.so and steam_api64.dll into ~/.local/share/luatools/goldberg/.",
            );
            guide_bullet(
                ui,
                "Steam Store 'LuaTools' button is blocked by Linux Steam CSP",
                "Linux Steam's Content Security Policy blocks standard theme JS injection. Either use the native Add Games tab directly, \
                 or click 'Install Store Page Plugin' + 'Enable Steam CEF Debugging' and tick 'Enable Steam Store CEF Bridge' in Settings › Steam Setup, then restart Steam.",
            );
            guide_bullet(
                ui,
                "DepotDownloader reports 'Access Denied' or 'No Subscription'",
                "Official SteamRE/DepotDownloader requires an authenticated Steam session for non-free depots — click 'Sign In (QR)' \
                 in Download Depots to cache your session, or install DepotDownloaderMod in Settings › Updates & Tools to use local .manifest files.",
            );
            guide_bullet(
                ui,
                "Launch Options or Steam Library Entry changes disappeared after quitting Steam",
                "Launch Options automatically coordinates shutting down and restarting Steam when saving. For Add / Remove Steam Library Entry (.acf), \
                 click 'Stop Steam' first, apply the change, and then click 'Launch Steam'.",
            );
        });

        ui.add_space(14.0);

        // Keyboard Shortcuts Table
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Keyboard Shortcuts")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.add_space(8.0);

            let shortcuts = [
                (
                    "Ctrl + K  /  Ctrl + L",
                    "Focus Global Search & Link Bar",
                ),
                (
                    "Ctrl + 1 … Ctrl + 5",
                    "Switch to Home, Add Games, My Games, Depots & Vault, or Game Fixes",
                ),
                ("Ctrl + 6  /  F1", "Open this Help manual"),
                ("Ctrl + 7  /  Ctrl + ,", "Open Preferences & Settings"),
                ("Esc", "Close active modal or screenshot preview"),
                ("Ctrl + Q", "Quit LuaTools Linux"),
            ];

            inset_paper_sheet(ui, Margin::same(10), |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(200.0))
                    .column(Column::remainder())
                    .body(|mut body| {
                        for (keys, desc) in shortcuts {
                            body.row(30.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(keys)
                                            .font(FontId::new(12.0, FontFamily::Monospace))
                                            .color(Palette::BLUE_LIGHT),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(desc).size(12.5).color(Palette::TEXT_PRIMARY),
                                    );
                                });
                            });
                        }
                    });
            });
        });

        ui.add_space(14.0);

        // Bug Reporting & Telemetry Quick Card
        let mut copy_diag = false;
        let mut report_gh = false;
        let mut open_tel = false;
        let tel_on = self.telemetry_service.is_enabled();

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Diagnostics, Telemetry & Bug Fixing")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if tel_on {
                        status_badge(ui, "Telemetry Enabled", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "Telemetry Off", BadgeTone::Muted);
                    }
                },
            );
            ui.label(
                RichText::new(
                    "When enabled, LuaTools records scrubbed diagnostic events (OS/display info, error codes, and crash locations — \
                     never usernames, tokens, API keys, or depot keys) so bugs can be diagnosed quickly. You can inspect, export, \
                     clear, or turn off telemetry anytime.",
                )
                .size(12.5)
                .color(Palette::TEXT_SECONDARY),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if primary_button(ui, "Report Bug on GitHub ›").clicked() {
                    report_gh = true;
                }
                if secondary_button(ui, "Copy Diagnostic Report").clicked() {
                    copy_diag = true;
                }
                if ghost_button(ui, "Manage / Turn Off Telemetry ›").clicked() {
                    open_tel = true;
                }
            });
        });

        if copy_diag {
            let md = self.telemetry_service.build_diagnostic_markdown(50);
            ui.ctx().copy_text(md);
            self.notify(
                "Copied scrubbed diagnostic report to clipboard.",
                ToastKind::Success,
            );
        }
        if report_gh {
            match self.telemetry_service.open_github_bug_report() {
                Ok(m) => self.notify(m, ToastKind::Info),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if open_tel {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Telemetry;
        }
    }
}

fn guide_step_card<F>(ui: &mut egui::Ui, badge: &str, title: &str, body: &str, actions: F)
where
    F: FnOnce(&mut egui::Ui),
{
    paper_card_padded(ui, Margin::same(18), |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            status_badge(ui, badge, BadgeTone::Blue);
            ui.label(
                RichText::new(title)
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
        });
        ui.label(
            RichText::new(body)
                .size(13.0)
                .color(Palette::TEXT_SECONDARY),
        );
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            actions(ui);
        });
    });
}

fn guide_bullet(ui: &mut egui::Ui, heading: &str, detail: &str) {
    inset_paper_sheet(ui, Margin::symmetric(12, 9), |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.label(
            RichText::new(heading)
                .font(semibold_font(13.0))
                .color(Palette::BLUE_LIGHT),
        );
        ui.label(
            RichText::new(detail)
                .size(12.5)
                .color(Palette::TEXT_SECONDARY),
        );
    });
}
