//! Primary Views: `01 // Overview`, `02 // Acquire`, and `03 // Library`.

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use eframe::egui::{
    self, Align, Color32, Frame, Layout, Margin, ProgressBar, RichText, ScrollArea, Stroke,
    TextEdit, TextureHandle, Vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::parsers::lua_parser::LuaFileParser;
use crate::parsers::steam_link::SteamLinkParser;
use crate::parsers::vdf_binary::LaunchOptionsService;
use crate::ui::app::LuaToolsApp;
use crate::ui::state::{AcquireTab, ActiveView, AppMessage};
use crate::ui::theme::{
    chip_button, danger_button, ghost_button, metric_tile, panel_frame, pick_files_native,
    primary_button, secondary_button, section_header, status_badge, table_button, BadgeTone,
    Palette,
};

impl LuaToolsApp {
    pub fn render_drop_zone_bar(&mut self, ui: &mut egui::Ui, compact: bool) {
        let pad_y = if compact { 9 } else { 13 };
        let resp = Frame::new()
            .fill(Color32::from_rgb(11, 14, 23))
            .stroke(Stroke::new(1.0_f32, Palette::BLUE_BORDER))
            .inner_margin(Margin::symmetric(14, pad_y))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("INGEST // DROP TARGET")
                                .monospace()
                                .strong()
                                .size(10.5)
                                .color(Palette::BLUE_LIGHT),
                        );
                        ui.label(
                            RichText::new(
                                "Drag & drop .lua, .manifest, .zip archives anywhere or paste a Steam / SteamDB URL",
                            )
                            .size(12.0)
                            .color(Palette::TEXT_SECONDARY),
                        );
                    });
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            TextEdit::singleline(&mut self.dropzone_url_input)
                                .hint_text(
                                    "Enter numeric AppID (e.g. 1245620) or https://store.steampowered.com/app/...",
                                )
                                .desired_width(ui.available_width() - 255.0),
                        );
                        let enter =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter || primary_button(ui, "INSPECT / LOAD").clicked() {
                            let raw = self.dropzone_url_input.trim().to_string();
                            if !raw.is_empty() {
                                let p = PathBuf::from(&raw);
                                if p.is_file() {
                                    self.dropzone_url_input.clear();
                                    self.install_dropped_paths(&[p]);
                                } else if let Some(aid) = SteamLinkParser::app_id_from(&raw) {
                                    self.dropzone_url_input.clear();
                                    self.open_app_in_acquire(aid, ui.ctx());
                                } else {
                                    self.status_text =
                                        "Invalid AppID or Steam URL in Ingest input.".into();
                                }
                            }
                        }
                        if secondary_button(ui, "SELECT FILES...").clicked() {
                            let tx = self.msg_tx.clone();
                            let ctx = ui.ctx().clone();
                            thread::spawn(move || {
                                let files = pick_files_native(
                                    "Select Lua, Manifest, or Zip Files",
                                    Some("*.lua *.manifest *.zip"),
                                );
                                if !files.is_empty() {
                                    let _ = tx.send(AppMessage::FilesPickedForIngest(files));
                                    ctx.request_repaint();
                                }
                            });
                        }
                    });
                });
            });

        let r = resp.response.rect;
        ui.painter().line_segment(
            [r.left_top(), r.left_bottom()],
            Stroke::new(3.0_f32, Palette::BLUE_HOVER),
        );
    }

    // ===== 01 // OVERVIEW =====

    pub fn render_overview(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .id_salt("overview_scroll")
            .show(ui, |ui| {
                let mut go_add = false;
                let mut open_new_lua = false;
                let mut do_donate = false;
                let mut do_refresh = false;

                section_header(
                    ui,
                    "01",
                    "SYSTEM OVERVIEW",
                    "Real-time environment telemetry, package ingestion, and recently configured titles",
                    |ui| {
                        if ghost_button(ui, "REFRESH").clicked() {
                            do_refresh = true;
                        }
                        if secondary_button(ui, "DONATE KEYS").clicked() {
                            do_donate = true;
                        }
                        if secondary_button(ui, "+ NEW LUA").clicked() {
                            open_new_lua = true;
                        }
                        if primary_button(ui, "BROWSE & ACQUIRE").clicked() {
                            go_add = true;
                        }
                    },
                );

                if go_add {
                    self.active_view = ActiveView::Acquire;
                }
                if open_new_lua {
                    self.add_appid_modal.open = true;
                }
                if do_refresh {
                    self.refresh_all_local_state();
                    self.probe_bridge_async(ui.ctx());
                }
                if do_donate {
                    self.trigger_donate_keys(ui.ctx());
                }

                ui.add_space(10.0);

                // 4 Telemetry Tiles
                let lua_total = self.recent_installed.len();
                let mut nav_target: Option<ActiveView> = None;

                ui.columns(4, |cols| {
                    // 1. Steam Engine
                    if let Some(inst) = &self.steam_service.installation {
                        let detail = format!(
                            "{}\n{} library folder(s)",
                            inst.root.display(),
                            inst.libraries.len()
                        );
                        if metric_tile(
                            &mut cols[0],
                            "STEAM ENGINE",
                            &format!("{} Configured .lua Game(s)", lua_total),
                            &detail,
                            "ONLINE",
                            BadgeTone::Blue,
                            "OPEN LIBRARY →",
                        ) {
                            nav_target = Some(ActiveView::Library);
                        }
                    } else if metric_tile(
                        &mut cols[0],
                        "STEAM ENGINE",
                        "Steam Not Found",
                        "Set custom override path in System",
                        "OFFLINE",
                        BadgeTone::Danger,
                        "CONFIGURE →",
                    ) {
                        nav_target = Some(ActiveView::Settings);
                    }

                    // 2. Account
                    if self.auth_service.is_signed_in() {
                        let email = self.auth_service.email();
                        let det = if email.is_empty() {
                            "Authenticated Discord session".to_string()
                        } else {
                            email
                        };
                        if metric_tile(
                            &mut cols[1],
                            "LUATOOLS ACCOUNT",
                            &self.auth_service.display_name(),
                            &det,
                            "SIGNED IN",
                            BadgeTone::Blue,
                            "ACCOUNT AUTH →",
                        ) {
                            nav_target = Some(ActiveView::Settings);
                        }
                    } else if metric_tile(
                        &mut cols[1],
                        "LUATOOLS ACCOUNT",
                        "Guest Mode",
                        "Sign in via Discord or Hubcap key",
                        "GUEST",
                        BadgeTone::Neutral,
                        "SIGN IN →",
                    ) {
                        nav_target = Some(ActiveView::Settings);
                    }

                    // 3. SLSsteam
                    let cfg = &self.slssteam_service.config;
                    let (hl, det, btxt, btone) = if cfg.exists() {
                        (
                            format!(
                                "PlayNotOwned: {}",
                                if cfg.play_not_owned { "Enabled" } else { "Disabled" }
                            ),
                            format!(
                                "{} FakeAppIds · {} DepotKeys",
                                cfg.fake_app_ids.len(),
                                cfg.depot_keys.len()
                            ),
                            if cfg.play_not_owned { "READY" } else { "STANDBY" },
                            if cfg.play_not_owned {
                                BadgeTone::Blue
                            } else {
                                BadgeTone::Neutral
                            },
                        )
                    } else {
                        (
                            "No config.yaml Yet".to_string(),
                            "Created automatically on game install".to_string(),
                            "UNSET",
                            BadgeTone::Neutral,
                        )
                    };
                    if metric_tile(
                        &mut cols[2],
                        "SLSSTEAM UNLOCKER",
                        &hl,
                        &det,
                        btxt,
                        btone,
                        "CONFIGURE →",
                    ) {
                        nav_target = Some(ActiveView::Settings);
                    }

                    // 4. Bridge
                    let (b_hl, b_det, b_tag, b_tone) = if self.millennium_connected {
                        (
                            "Millennium Connected",
                            "RPC Bridge active on 127.0.0.1:38495".to_string(),
                            "RPC :38495",
                            BadgeTone::Blue,
                        )
                    } else if self.cef_injector.is_running() {
                        (
                            "CEF CDP Active",
                            self.cef_injector.last_status(),
                            "CEF :8080",
                            BadgeTone::Blue,
                        )
                    } else {
                        (
                            "Standalone GUI Mode",
                            "Direct stplug-in & depotcache active".to_string(),
                            "DIRECT",
                            BadgeTone::Neutral,
                        )
                    };
                    if metric_tile(
                        &mut cols[3],
                        "BRIDGE & INJECTOR",
                        b_hl,
                        &b_det,
                        b_tag,
                        b_tone,
                        "FIX CATALOG →",
                    ) {
                        nav_target = Some(ActiveView::Fixes);
                    }
                });

                if let Some(target) = nav_target {
                    self.active_view = target;
                }

                ui.add_space(12.0);
                self.render_drop_zone_bar(ui, false);
                ui.add_space(14.0);

                // Recently Configured Titles
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("RECENTLY CONFIGURED STPLUG-IN TITLES")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} CONFIGURED", lua_total))
                                .monospace()
                                .size(11.0)
                                .color(Palette::BLUE_LIGHT),
                        );
                    });
                });

                panel_frame().show(ui, |ui| {
                    if self.recent_installed.is_empty() {
                        ui.label(
                            RichText::new(
                                "No .lua configurations installed yet. Drop an archive above or browse 02 // ACQUIRE.",
                            )
                            .color(Palette::TEXT_MUTED),
                        );
                    } else {
                        let cards: Vec<u32> = self
                            .recent_installed
                            .iter()
                            .take(4)
                            .map(|(aid, _)| *aid)
                            .collect();

                        let mut open_depots = None;
                        let mut open_inspect = None;

                        ui.horizontal_wrapped(|ui| {
                            for aid in cards {
                                self.ensure_cover_requested(aid, None, ui.ctx());
                                let tex: Option<TextureHandle> =
                                    self.cover_textures.get(&aid).cloned();
                                let name = self.resolve_game_title(aid);

                                Frame::new()
                                    .fill(Palette::BG_VOID)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                                    .inner_margin(Margin::same(10))
                                    .show(ui, |ui| {
                                        ui.set_width(244.0);
                                        ui.vertical(|ui| {
                                            if let Some(t) = tex {
                                                ui.add(
                                                    egui::Image::new(&t)
                                                        .fit_to_exact_size(Vec2::new(224.0, 105.0)),
                                                );
                                            } else {
                                                let (rect, _) = ui.allocate_exact_size(
                                                    Vec2::new(224.0, 105.0),
                                                    egui::Sense::hover(),
                                                );
                                                ui.painter().rect_filled(
                                                    rect,
                                                    egui::CornerRadius::ZERO,
                                                    Palette::BG_PANEL,
                                                );
                                                ui.painter().text(
                                                    rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    format!("APP // {}", aid),
                                                    egui::FontId::monospace(11.0),
                                                    Palette::TEXT_MUTED,
                                                );
                                            }
                                            ui.add_space(4.0);
                                            ui.label(
                                                RichText::new(name)
                                                    .strong()
                                                    .size(13.0)
                                                    .color(Palette::TEXT_PRIMARY),
                                            );
                                            ui.label(
                                                RichText::new(format!("APPID {}", aid))
                                                    .monospace()
                                                    .size(10.5)
                                                    .color(Palette::BLUE_LIGHT),
                                            );
                                            ui.horizontal(|ui| {
                                                if table_button(ui, "DEPOTS", true).clicked() {
                                                    open_depots = Some(aid);
                                                }
                                                if table_button(ui, "INSPECT", false).clicked() {
                                                    open_inspect = Some(aid);
                                                }
                                            });
                                        });
                                    });
                            }
                        });

                        if let Some(aid) = open_depots {
                            self.open_game_in_depots(aid, ui.ctx());
                        }
                        if let Some(aid) = open_inspect {
                            self.open_app_in_acquire(aid, ui.ctx());
                        }
                    }
                });
            });
    }

    // ===== 02 // ACQUIRE =====

    pub fn render_acquire(&mut self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "02",
            "ACQUIRE & MANIFEST INSPECTOR",
            "Search Steam Store, check manifest availability across providers, and install directly to stplug-in",
            |_ui| {},
        );
        ui.add_space(6.0);

        // Search Bar
        ui.horizontal(|ui| {
            let edit = ui.add(
                TextEdit::singleline(&mut self.acquire_search_query)
                    .hint_text("Search Steam Store by title (e.g. Elden Ring, Warhammer 40000) or paste AppID / Steam URL...")
                    .desired_width(ui.available_width() - 210.0),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || primary_button(ui, "SEARCH STORE").clicked() {
                let q = self.acquire_search_query.trim().to_string();
                self.execute_store_search(&q, ui.ctx());
            }
            if ghost_button(ui, "RESET").clicked() {
                self.acquire_search_query.clear();
                self.acquire_tab = AcquireTab::TopSellers;
            }
        });

        // Category Chips
        ui.horizontal(|ui| {
            if chip_button(
                ui,
                &format!("TOP SELLERS ({})", self.featured_top.len()),
                self.acquire_tab == AcquireTab::TopSellers,
            )
            .clicked()
            {
                self.acquire_tab = AcquireTab::TopSellers;
            }
            if chip_button(
                ui,
                &format!("NEW RELEASES ({})", self.featured_new.len()),
                self.acquire_tab == AcquireTab::NewReleases,
            )
            .clicked()
            {
                self.acquire_tab = AcquireTab::NewReleases;
            }
            if chip_button(
                ui,
                &format!("SEARCH RESULTS ({})", self.search_results.len()),
                self.acquire_tab == AcquireTab::SearchResults,
            )
            .clicked()
            {
                self.acquire_tab = AcquireTab::SearchResults;
            }
        });

        let avail_h = (ui.available_height() - 95.0).max(260.0);
        let mut clicked_app: Option<u32> = None;

        ui.columns(2, |cols| {
            // Left: Store Results List
            Frame::new()
                .fill(Palette::BG_PANEL)
                .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                .inner_margin(Margin::same(8))
                .show(&mut cols[0], |ui| {
                    ui.set_height(avail_h);
                    ScrollArea::vertical()
                        .id_salt("acquire_list_scroll")
                        .show(ui, |ui| {
                            let entries: Vec<(u32, String)> = match self.acquire_tab {
                                AcquireTab::TopSellers => self
                                    .featured_top
                                    .iter()
                                    .map(|i| (i.app_id, i.name.clone()))
                                    .collect(),
                                AcquireTab::NewReleases => self
                                    .featured_new
                                    .iter()
                                    .map(|i| (i.app_id, i.name.clone()))
                                    .collect(),
                                AcquireTab::SearchResults => self
                                    .search_results
                                    .iter()
                                    .map(|i| (i.app_id, i.name.clone()))
                                    .collect(),
                            };

                            if entries.is_empty() {
                                ui.label(
                                    RichText::new("No items loaded yet.")
                                        .color(Palette::TEXT_MUTED),
                                );
                            }
                            for (aid, name) in entries {
                                let is_sel = self.acquire_selected_app == Some(aid);
                                let resp = ui.selectable_label(
                                    is_sel,
                                    format!("{}   [{}]", name, aid),
                                );
                                if resp.clicked() {
                                    clicked_app = Some(aid);
                                }
                            }
                        });
                });

            // Right: Game Inspector
            Frame::new()
                .fill(Palette::BG_PANEL)
                .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                .inner_margin(Margin::same(14))
                .show(&mut cols[1], |ui| {
                    ui.set_height(avail_h);
                    if let Some(aid) = self.acquire_selected_app {
                        let has_lua = self
                            .lua_installer
                            .as_ref()
                            .and_then(|i| i.read_installed_lua_path(aid))
                            .is_some();

                        ui.horizontal(|ui| {
                            if let Some(tex) = self.cover_textures.get(&aid) {
                                ui.add(
                                    egui::Image::new(tex)
                                        .fit_to_exact_size(Vec2::new(240.0, 112.0)),
                                );
                            } else {
                                let (rect, _) = ui.allocate_exact_size(
                                    Vec2::new(240.0, 112.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::ZERO,
                                    Palette::BG_VOID,
                                );
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    format!("APP // {}", aid),
                                    egui::FontId::monospace(11.0),
                                    Palette::TEXT_MUTED,
                                );
                            }

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    if has_lua {
                                        status_badge(ui, "LUA CONFIGURED", BadgeTone::Blue);
                                        if table_button(ui, "OPEN IN DEPOTS →", true).clicked() {
                                            self.open_game_in_depots(aid, ui.ctx());
                                        }
                                    } else {
                                        status_badge(ui, "NOT INSTALLED", BadgeTone::Neutral);
                                    }
                                });
                                let title = self
                                    .acquire_details
                                    .as_ref()
                                    .map(|d| d.name.as_str())
                                    .unwrap_or("Loading...");
                                ui.label(
                                    RichText::new(title)
                                        .strong()
                                        .size(16.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                                ui.label(
                                    RichText::new(format!("APPID // {}", aid))
                                        .monospace()
                                        .strong()
                                        .color(Palette::BLUE_LIGHT),
                                );
                                if let Some(d) = &self.acquire_details {
                                    let mut meta = vec![if d.app_type.is_empty() {
                                        "GAME".to_string()
                                    } else {
                                        d.app_type.to_ascii_uppercase()
                                    }];
                                    if !d.release_date.is_empty() {
                                        meta.push(d.release_date.clone());
                                    }
                                    if !d.genres.is_empty() {
                                        meta.push(d.genres.iter().take(3).cloned().collect::<Vec<_>>().join(" / "));
                                    }
                                    ui.label(
                                        RichText::new(meta.join("  ·  "))
                                            .size(11.5)
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                }
                            });
                        });

                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("MANIFEST PROVIDERS & AVAILABILITY")
                                .monospace()
                                .strong()
                                .size(11.0)
                                .color(Palette::TEXT_SECONDARY),
                        );

                        let mut avail_providers: Vec<String> = Vec::new();
                        Frame::new()
                            .fill(Palette::BG_VOID)
                            .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                                ui.set_min_height(90.0);
                                if self.acquire_sources.is_empty() {
                                    ui.label(
                                        RichText::new(
                                            "Checking providers (or offline — drop a local .lua/.zip below)...",
                                        )
                                        .monospace()
                                        .size(11.0)
                                        .color(Palette::TEXT_MUTED),
                                    );
                                } else {
                                    for (prov, st) in &self.acquire_sources {
                                        let is_av = st.eq_ignore_ascii_case("available");
                                        if is_av {
                                            avail_providers.push(prov.clone());
                                        }
                                        let is_sel = self
                                            .acquire_selected_source
                                            .as_deref()
                                            == Some(prov.as_str());
                                        let line = format!(
                                            "{}  {:<16}  ::  {}",
                                            if is_av { "[●]" } else { "[○]" },
                                            prov.to_ascii_uppercase(),
                                            st.to_ascii_uppercase()
                                        );
                                        let color = if is_av {
                                            Palette::BLUE_LIGHT
                                        } else {
                                            Palette::TEXT_MUTED
                                        };
                                        if ui
                                            .selectable_label(
                                                is_sel,
                                                RichText::new(line).monospace().color(color),
                                            )
                                            .clicked()
                                        {
                                            self.acquire_selected_source = Some(prov.clone());
                                        }
                                    }
                                }
                            });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(
                                !avail_providers.is_empty() && !self.acquire_busy,
                                |ui| {
                                    if primary_button(ui, "FASTFETCH & INSTALL").clicked() {
                                        if let Some(first) = avail_providers.first() {
                                            self.start_acquire_download(aid, first.clone(), ui.ctx());
                                        }
                                    }
                                    if secondary_button(ui, "INSTALL SOURCE").clicked() {
                                        let chosen = self
                                            .acquire_selected_source
                                            .clone()
                                            .or_else(|| avail_providers.first().cloned());
                                        if let Some(src) = chosen {
                                            if avail_providers.contains(&src) {
                                                self.start_acquire_download(aid, src, ui.ctx());
                                            } else {
                                                self.acquire_status =
                                                    format!("Provider '{}' is unavailable.", src);
                                            }
                                        }
                                    }
                                },
                            );

                            if let Some(dlc) = &self.acquire_dlc_info {
                                if dlc.missing_count > 0
                                    && secondary_button(
                                        ui,
                                        &format!("DLC LUA (+{})", dlc.missing_count),
                                    )
                                    .clicked()
                                {
                                    self.start_dlc_generation(aid, ui.ctx());
                                }
                            }

                            if ghost_button(ui, "STORE ↗").clicked() {
                                let _ = Command::new("xdg-open")
                                    .arg(format!("https://store.steampowered.com/app/{}", aid))
                                    .spawn();
                            }
                        });

                        if self.acquire_busy {
                            ui.add(
                                ProgressBar::new(self.acquire_progress)
                                    .show_percentage()
                                    .corner_radius(egui::CornerRadius::ZERO),
                            );
                        }
                        ui.label(
                            RichText::new(&self.acquire_status)
                                .monospace()
                                .size(11.0)
                                .color(Palette::TEXT_MUTED),
                        );
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new(
                                    "SELECT A TITLE FROM THE LEFT OR PASTE AN APPID / URL ABOVE",
                                )
                                .monospace()
                                .color(Palette::TEXT_MUTED),
                            );
                        });
                    }
                });
        });

        if let Some(aid) = clicked_app {
            self.open_app_in_acquire(aid, ui.ctx());
        }

        ui.add_space(6.0);
        self.render_drop_zone_bar(ui, true);
    }

    // ===== 03 // LIBRARY =====

    pub fn render_library(&mut self, ui: &mut egui::Ui) {
        let mut open_add = false;
        let mut do_bulk_del = false;
        let mut do_refresh = false;

        section_header(
            ui,
            "03",
            "GAME LIBRARY & OVERRIDES",
            "Inspect installed Steam titles, stplug-in .lua manifests, FakeAppIds, tokens, and appinfo.vdf launch options",
            |ui| {
                if ghost_button(ui, "REFRESH").clicked() {
                    do_refresh = true;
                }
                if danger_button(ui, "DELETE SELECTED LUA").clicked() {
                    do_bulk_del = true;
                }
                if primary_button(ui, "+ ADD BY APPID").clicked() {
                    open_add = true;
                }
            },
        );

        if open_add {
            self.add_appid_modal.open = true;
        }
        if do_refresh {
            self.refresh_library_rows();
        }
        if do_bulk_del && !self.library_selected_ids.is_empty() {
            if let Some(installer) = self.lua_installer.clone() {
                let ids: Vec<u32> = self.library_selected_ids.iter().copied().collect();
                for aid in ids {
                    let _ = installer.uninstall_lua(aid, false, Some(&mut self.slssteam_service));
                }
                self.library_selected_ids.clear();
                self.refresh_all_local_state();
            }
        }

        ui.add_space(6.0);
        let total_n = self.library_rows.len();
        let lua_n = self.library_rows.iter().filter(|r| r.has_lua).count();
        let inst_n = self.library_rows.iter().filter(|r| r.installed).count();
        let over_n = self
            .library_rows
            .iter()
            .filter(|r| r.fake_id.is_some() || r.has_token)
            .count();

        ui.horizontal(|ui| {
            if chip_button(
                ui,
                &format!("ALL ({})", total_n),
                self.library_filter_mode == 0,
            )
            .clicked()
            {
                self.library_filter_mode = 0;
            }
            if chip_button(
                ui,
                &format!("LUA ACTIVE ({})", lua_n),
                self.library_filter_mode == 1,
            )
            .clicked()
            {
                self.library_filter_mode = 1;
            }
            if chip_button(
                ui,
                &format!("INSTALLED ({})", inst_n),
                self.library_filter_mode == 2,
            )
            .clicked()
            {
                self.library_filter_mode = 2;
            }
            if chip_button(
                ui,
                &format!("OVERRIDES ({})", over_n),
                self.library_filter_mode == 3,
            )
            .clicked()
            {
                self.library_filter_mode = 3;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    TextEdit::singleline(&mut self.library_search)
                        .hint_text("Filter library by title or AppID...")
                        .desired_width(280.0),
                );
            });
        });

        let q = self.library_search.trim().to_ascii_lowercase();
        let filtered: Vec<_> = self
            .library_rows
            .iter()
            .filter(|r| match self.library_filter_mode {
                1 => r.has_lua,
                2 => r.installed,
                3 => r.fake_id.is_some() || r.has_token,
                _ => true,
            })
            .filter(|r| {
                q.is_empty()
                    || r.name.to_ascii_lowercase().contains(&q)
                    || r.app_id.to_string().contains(&q)
            })
            .cloned()
            .collect();

        let mut act_config: Option<(u32, String)> = None;
        let mut act_depots: Option<u32> = None;
        let mut act_launch: Option<(u32, String)> = None;
        let mut act_fixes: Option<u32> = None;

        Frame::new()
            .fill(Palette::BG_PANEL)
            .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(36.0))
                    .column(Column::exact(88.0))
                    .column(Column::remainder().at_least(180.0))
                    .column(Column::exact(135.0))
                    .column(Column::exact(68.0))
                    .column(Column::exact(72.0))
                    .column(Column::exact(64.0))
                    .column(Column::exact(355.0))
                    .header(28.0, |mut header| {
                        for h in [
                            "SEL", "APP ID", "TITLE", "STATE", "LUA", "FAKE ID", "TOKEN", "ACTIONS",
                        ] {
                            header.col(|ui| {
                                ui.label(
                                    RichText::new(h)
                                        .monospace()
                                        .strong()
                                        .size(11.0)
                                        .color(Palette::TEXT_MUTED),
                                );
                            });
                        }
                    })
                    .body(|mut body| {
                        for r in &filtered {
                            body.row(36.0, |mut row| {
                                row.col(|ui| {
                                    let mut checked = self.library_selected_ids.contains(&r.app_id);
                                    if ui.checkbox(&mut checked, "").changed() {
                                        if checked {
                                            self.library_selected_ids.insert(r.app_id);
                                        } else {
                                            self.library_selected_ids.remove(&r.app_id);
                                        }
                                    }
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(r.app_id.to_string())
                                            .monospace()
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                                row.col(|ui| {
                                    if ui
                                        .selectable_label(false, RichText::new(&r.name).strong())
                                        .double_clicked()
                                    {
                                        act_config = Some((r.app_id, r.name.clone()));
                                    }
                                });
                                row.col(|ui| {
                                    let col = if r.has_lua {
                                        Palette::BLUE_LIGHT
                                    } else {
                                        Palette::TEXT_SECONDARY
                                    };
                                    ui.label(
                                        RichText::new(r.status_label)
                                            .monospace()
                                            .strong()
                                            .size(11.0)
                                            .color(col),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(if r.has_lua { "ACTIVE" } else { "—" })
                                            .monospace()
                                            .color(if r.has_lua {
                                                Palette::BLUE_LIGHT
                                            } else {
                                                Palette::TEXT_MUTED
                                            }),
                                    );
                                });
                                row.col(|ui| {
                                    let txt = r
                                        .fake_id
                                        .map(|f| f.to_string())
                                        .unwrap_or_else(|| "—".into());
                                    ui.label(RichText::new(txt).monospace());
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(if r.has_token { "SET" } else { "—" })
                                            .monospace()
                                            .color(if r.has_token {
                                                Palette::BLUE_LIGHT
                                            } else {
                                                Palette::TEXT_MUTED
                                            }),
                                    );
                                });
                                row.col(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    if table_button(ui, "CONFIG", false).clicked() {
                                        act_config = Some((r.app_id, r.name.clone()));
                                    }
                                    if table_button(ui, "DEPOTS", r.has_lua).clicked() {
                                        act_depots = Some(r.app_id);
                                    }
                                    if table_button(ui, "LAUNCH", false).clicked() {
                                        act_launch = Some((r.app_id, r.name.clone()));
                                    }
                                    if table_button(ui, "FIXES", false).clicked() {
                                        act_fixes = Some(r.app_id);
                                    }
                                    if let Some(p) = &r.install_path {
                                        if p.exists() && table_button(ui, "DIR", false).clicked() {
                                            let _ = Command::new("xdg-open").arg(p).spawn();
                                        }
                                    }
                                    if table_button(ui, "STORE", false).clicked() {
                                        let _ = Command::new("xdg-open")
                                            .arg(format!(
                                                "https://store.steampowered.com/app/{}",
                                                r.app_id
                                            ))
                                            .spawn();
                                    }
                                });
                            });
                        }
                    });
            });

        if let Some((aid, name)) = act_config {
            self.open_game_detail_modal(aid, &name);
        }
        if let Some(aid) = act_depots {
            self.open_game_in_depots(aid, ui.ctx());
        }
        if let Some((aid, name)) = act_launch {
            self.open_launch_options_modal(aid, &name);
        }
        if let Some(aid) = act_fixes {
            self.open_app_in_fixes(aid, ui.ctx());
        }
    }

    pub fn open_game_detail_modal(&mut self, app_id: u32, name: &str) {
        let lua_text = self
            .lua_installer
            .as_ref()
            .and_then(|i| i.read_lua_text(app_id))
            .unwrap_or_else(|| LuaFileParser::default_lua_template(app_id, name));
        let fake = self
            .slssteam_service
            .get_fake_app_id(app_id)
            .map(|x| x.to_string())
            .unwrap_or_default();
        let tok = self
            .slssteam_service
            .get_token(app_id)
            .unwrap_or_default()
            .to_string();

        self.game_detail_modal.open = true;
        self.game_detail_modal.app_id = app_id;
        self.game_detail_modal.game_name = name.to_string();
        self.game_detail_modal.lua_text = lua_text;
        self.game_detail_modal.fake_id_input = fake;
        self.game_detail_modal.token_input = tok;
        self.game_detail_modal.show_token = false;
        self.game_detail_modal.quick_depot_input.clear();
        self.game_detail_modal.feedback.clear();
    }

    pub fn open_launch_options_modal(&mut self, app_id: u32, name: &str) {
        let entries = self
            .steam_service
            .installation
            .as_ref()
            .map(|i| LaunchOptionsService::new(i.root.clone()).get_launch_entries(app_id))
            .unwrap_or_default();

        self.launch_options_modal.selected_idx = (!entries.is_empty()).then_some(0);
        self.launch_options_modal.open = true;
        self.launch_options_modal.app_id = app_id;
        self.launch_options_modal.game_name = name.to_string();
        self.launch_options_modal.entries = entries;
        self.launch_options_modal.feedback.clear();
    }
}
