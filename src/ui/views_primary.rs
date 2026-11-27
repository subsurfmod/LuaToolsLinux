//! Primary Workspaces: `Home` (`Overview`), `Discover` (`Acquire`), and `Library`.
//!
//! Designed with Papercut layered construction-paper cards, generous 8pt spacing,
//! rich serif display titles, and `StripBuilder` / `Sides` zero-overlap layouts.

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use eframe::egui::{
    self, Align, Checkbox, CornerRadius, FontFamily, FontId, Layout, Margin, ProgressBar, RichText,
    ScrollArea, Sense, TextEdit, TextureHandle, Vec2,
};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use crate::parsers::vdf_binary::LaunchOptionsService;
use crate::ui::app::LuaToolsApp;
use crate::ui::state::{AcquireTab, ActiveView, AppMessage, ToastKind};
use crate::ui::theme::{
    chip_button, danger_button, display_font, draw_rem_medallion, ghost_button, inset_paper_sheet,
    paper_card, paper_card_padded, pick_files_native, primary_button, secondary_button,
    section_header, semibold_font, status_badge, table_button, BadgeTone, Palette,
};

impl LuaToolsApp {
    // ===== 1. HOME (OVERVIEW) =====

    pub fn render_overview(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .id_salt("home_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut go_discover = false;
                let mut go_guide = false;

                section_header(
                    ui,
                    "HOME",
                    "lets start crackin 😛",
                    "Add Steam games in one click or drop game files below",
                    |ui| {
                        if ghost_button(ui, "Help")
                            .on_hover_text("Open the illustrated user guide and troubleshooting tips (F1)")
                            .clicked()
                        {
                            go_guide = true;
                        }
                        if primary_button(ui, "Add Games")
                            .on_hover_text("Search Steam games and install .lua manifests from cloud providers")
                            .clicked()
                        {
                            go_discover = true;
                        }
                    },
                );

                if go_discover {
                    self.active_view = ActiveView::Acquire;
                }
                if go_guide {
                    self.active_view = ActiveView::Guide;
                }

                // 1. Tactile Cut-Paper Drop Sheet (simple, plain-English)
                paper_card_padded(ui, Margin::symmetric(24, 22), |ui| {
                    egui::Sides::new().show(
                        ui,
                        |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 16.0;
                                draw_rem_medallion(ui, 46.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.label(
                                        RichText::new("Drop game files here (.lua, .manifest, or .zip)")
                                            .font(display_font(17.5))
                                            .color(Palette::TEXT_PRIMARY),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Files install automatically and sync with Steam",
                                        )
                                        .size(13.0)
                                        .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                            });
                        },
                        |ui| {
                            if primary_button(ui, "Browse Files…")
                                .on_hover_text("Pick local .lua, .manifest, or .zip packages from disk")
                                .clicked()
                            {
                                let tx = self.msg_tx.clone();
                                let ctx = ui.ctx().clone();
                                thread::spawn(move || {
                                    let files = pick_files_native(
                                        "Select Game Files (.lua, .manifest, .zip)",
                                        Some("*.lua *.manifest *.zip"),
                                    );
                                    if !files.is_empty() {
                                        let _ = tx.send(AppMessage::FilesPickedForIngest(files));
                                        ctx.request_repaint();
                                    }
                                });
                            }
                        },
                    );
                });

                ui.add_space(16.0);

                // 2. Calm 3-Card Status Summary (clickable to open setup directly)
                let steam_headline = match &self.steam_service.installation {
                    Some(i) => format!("{} Steam library folder(s)", i.libraries.len()),
                    None => "Steam not found".into(),
                };
                let steam_sub = format!("{} game(s) added", self.recent_installed.len());
                let steam_tone = if self.steam_service.is_available() {
                    BadgeTone::Ready
                } else {
                    BadgeTone::Warn
                };

                let sls_cfg = &self.slssteam_service.config;
                let bridge_st = self.steam_bridge_service.status();
                let unlocker_ready = self.steam_sls_installed
                    || bridge_st.integration_lib_installed
                    || bridge_st.launcher_installed
                    || bridge_st.system_preload_enabled;
                let sls_headline = if unlocker_ready {
                    format!("{} game(s) unlocked", sls_cfg.additional_apps.len())
                } else {
                    "Click for 1-click setup".into()
                };
                let sls_sub = if unlocker_ready {
                    format!(
                        "Play unowned games: {}",
                        if sls_cfg.play_not_owned { "On" } else { "Off" }
                    )
                } else {
                    "Needed to play added games in Steam".into()
                };

                let acct_headline = if self.auth_service.is_signed_in() {
                    self.auth_service.display_name()
                } else {
                    "Not signed in".into()
                };
                let acct_sub = if !self.settings_usage_text.is_empty() {
                    self.settings_usage_text.clone()
                } else if self.auth_service.is_signed_in() {
                    "Signed in with Discord".into()
                } else {
                    "Click to sign in for cloud downloads".into()
                };

                let mut click_steam_tile = false;
                let mut click_unlocker_tile = false;
                let mut click_account_tile = false;

                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 98.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        StripBuilder::new(ui)
                            .size(Size::relative(0.32))
                            .size(Size::exact(14.0))
                            .size(Size::relative(0.32))
                            .size(Size::exact(14.0))
                            .size(Size::remainder())
                            .horizontal(|mut strip| {
                                strip.cell(|ui| {
                                    if summary_paper_tile(
                                        ui,
                                        "STEAM",
                                        &steam_headline,
                                        &steam_sub,
                                        if self.steam_service.is_available() {
                                            "Connected"
                                        } else {
                                            "Offline"
                                        },
                                        steam_tone,
                                    )
                                    .on_hover_text(
                                        "Click to open Settings › General (Steam folder path, rescan, login autostart & system tray)",
                                    )
                                    .clicked()
                                    {
                                        click_steam_tile = true;
                                    }
                                });
                                strip.empty();
                                strip.cell(|ui| {
                                    if summary_paper_tile(
                                        ui,
                                        "STEAM UNLOCKER",
                                        &sls_headline,
                                        &sls_sub,
                                        if unlocker_ready { "Ready" } else { "Setup" },
                                        if unlocker_ready {
                                            BadgeTone::Ready
                                        } else {
                                            BadgeTone::Warn
                                        },
                                    )
                                    .on_hover_text(
                                        "Click to open Settings › Steam Setup (1-click Steam Unlocker installer, Play Unowned & Cloud Save Redirect)",
                                    )
                                    .clicked()
                                    {
                                        click_unlocker_tile = true;
                                    }
                                });
                                strip.empty();
                                strip.cell(|ui| {
                                    if summary_paper_tile(
                                        ui,
                                        "ACCOUNT",
                                        &acct_headline,
                                        &acct_sub,
                                        if self.auth_service.is_signed_in() {
                                            "Active"
                                        } else {
                                            "Sign In"
                                        },
                                        if self.auth_service.is_signed_in() {
                                            BadgeTone::Blue
                                        } else {
                                            BadgeTone::Neutral
                                        },
                                    )
                                    .on_hover_text(
                                        "Click to open Settings › Account (Discord sign-in, Hubcap API key & DepotDownloader QR login)",
                                    )
                                    .clicked()
                                    {
                                        click_account_tile = true;
                                    }
                                });
                            });
                    },
                );

                if click_steam_tile {
                    self.active_view = ActiveView::Settings;
                    self.settings_subtab = crate::ui::state::SettingsSubTab::General;
                }
                if click_unlocker_tile {
                    self.active_view = ActiveView::Settings;
                    self.settings_subtab = crate::ui::state::SettingsSubTab::Integrations;
                }
                if click_account_tile {
                    self.active_view = ActiveView::Settings;
                    self.settings_subtab = crate::ui::state::SettingsSubTab::Account;
                }

                ui.add_space(20.0);

                // 3. Recently Added Games
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new("Recently Added Games")
                                    .font(display_font(17.5))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(
                                    "Click Open on any game for 1-click Recommended Fixes & Integration, or Depots & DLC to manage files",
                                )
                                .size(12.0)
                                .color(Palette::TEXT_MUTED),
                            );
                        });
                    },
                    |ui| {
                        if !self.library_rows.is_empty()
                            && ghost_button(ui, "View All Games ›")
                                .on_hover_text("Open My Games to see all added and installed titles")
                                .clicked()
                        {
                            self.active_view = ActiveView::Library;
                        }
                    },
                );
                ui.add_space(10.0);

                let cards: Vec<u32> = if !self.recent_installed.is_empty() {
                    self.recent_installed
                        .iter()
                        .take(8)
                        .map(|(aid, _)| *aid)
                        .collect()
                } else {
                    self.library_rows
                        .iter()
                        .filter(|r| r.has_lua)
                        .take(8)
                        .map(|r| r.app_id)
                        .collect()
                };

                if cards.is_empty() {
                    paper_card_padded(ui, Margin::symmetric(24, 28), |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical_centered(|ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            ui.label(
                                RichText::new("No games added yet")
                                    .font(semibold_font(14.5))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(
                                    "Click Add Games above, search a title in the top bar, or drop a game file here.",
                                )
                                .size(13.0)
                                .color(Palette::TEXT_MUTED),
                            );
                        });
                    });
                } else {
                    let mut open_integration = None;
                    let mut open_depots = None;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(14.0, 14.0);
                        for aid in cards {
                            let known_url = self.cover_known_urls.get(&aid).cloned();
                            self.ensure_cover_requested(aid, known_url, ui.ctx());
                            let tex: Option<TextureHandle> =
                                self.cover_textures.get(&aid).cloned();
                            let loading_cover = self.cover_in_flight.contains(&aid);
                            let name = self.resolve_game_title(aid);
                            let is_installed = self
                                .library_rows
                                .iter()
                                .any(|r| r.app_id == aid && r.installed);

                            paper_card_padded(ui, Margin::same(12), |ui| {
                                ui.set_width(228.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 6.0;
                                    let cover_resp = if let Some(t) = tex {
                                        ui.add(
                                            egui::Image::new(&t)
                                                .fit_to_exact_size(Vec2::new(228.0, 106.0))
                                                .corner_radius(CornerRadius::same(3))
                                                .sense(Sense::click()),
                                        )
                                    } else {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(228.0, 106.0),
                                            Sense::click(),
                                        );
                                        ui.painter().rect_filled(
                                            rect,
                                            CornerRadius::same(3),
                                            Palette::BG_VOID,
                                        );
                                        let placeholder = if loading_cover {
                                            format!("Loading #{}…", aid)
                                        } else {
                                            format!("#{}", aid)
                                        };
                                        ui.painter().text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            placeholder,
                                            FontId::new(13.0, FontFamily::Monospace),
                                            Palette::TEXT_MUTED,
                                        );
                                        resp
                                    };
                                    if cover_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if cover_resp
                                        .on_hover_text(
                                            "Click to open Integration & Process Controls for this game",
                                        )
                                        .clicked()
                                    {
                                        open_integration = Some(aid);
                                    }
                                    let title_resp = ui.label(
                                        RichText::new(truncate_label(&name, 26))
                                            .font(semibold_font(13.5))
                                            .color(Palette::TEXT_PRIMARY),
                                    );
                                    if name.chars().count() > 26 {
                                        title_resp.on_hover_text(&name);
                                    }
                                    egui::Sides::new().show(
                                        ui,
                                        |ui| {
                                            ui.label(
                                                RichText::new(format!("#{}", aid))
                                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                                    .color(Palette::TEXT_MUTED),
                                            );
                                        },
                                        |ui| {
                                            ui.label(
                                                RichText::new(if is_installed {
                                                    "Installed + Lua"
                                                } else {
                                                    "Added (.lua)"
                                                })
                                                .size(11.0)
                                                .color(if is_installed {
                                                    Palette::GO_TEXT
                                                } else {
                                                    Palette::BLUE_LIGHT
                                                }),
                                            );
                                        },
                                    );
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        if table_button(ui, "Open", true)
                                            .on_hover_text(
                                                "Open Integration & Process Controls (1-click Recommended Fixes, Play Unowned, AppID 480 redirect, Goldberg & game tools)",
                                            )
                                            .clicked()
                                        {
                                            open_integration = Some(aid);
                                        }
                                        if table_button(ui, "Depots & DLC", false)
                                            .on_hover_text(
                                                "Open Depots & Vault to toggle DLCs, switch saved .lua builds, pin manifests, or download depots",
                                            )
                                            .clicked()
                                        {
                                            open_depots = Some(aid);
                                        }
                                    });
                                });
                            });
                        }
                    });
                    if let Some(aid) = open_integration {
                        self.open_integration_for_app(aid);
                    }
                    if let Some(aid) = open_depots {
                        self.open_game_in_depots(aid, ui.ctx());
                    }
                }
                ui.add_space(20.0);
            });
    }

    // ===== 2. DISCOVER (ACQUIRE) =====

    pub fn render_acquire(&mut self, ui: &mut egui::Ui) {
        let mut do_refresh_catalog = false;
        section_header(
            ui,
            "ADD GAMES",
            "Find & Add Steam Games",
            "Search any Steam game or browse popular releases",
            |ui| {
                if ghost_button(ui, "Refresh").clicked() {
                    do_refresh_catalog = true;
                }
            },
        );
        if do_refresh_catalog {
            self.featured_requested = true;
            self.fetch_featured_async(ui.ctx());
        }

        // Single Clean Filter & Search Toolbar
        let mut trigger_search = false;
        egui::Sides::new().show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if chip_button(
                    ui,
                    &format!("Top Sellers ({})", self.featured_top.len()),
                    self.acquire_tab == AcquireTab::TopSellers,
                )
                .clicked()
                {
                    self.acquire_tab = AcquireTab::TopSellers;
                }
                if chip_button(
                    ui,
                    &format!("New Releases ({})", self.featured_new.len()),
                    self.acquire_tab == AcquireTab::NewReleases,
                )
                .clicked()
                {
                    self.acquire_tab = AcquireTab::NewReleases;
                }
                if chip_button(
                    ui,
                    &format!("Search ({})", self.search_results.len()),
                    self.acquire_tab == AcquireTab::SearchResults,
                )
                .clicked()
                {
                    self.acquire_tab = AcquireTab::SearchResults;
                }
            },
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Note: Sides right closure uses right_to_left layout, so add rightmost widget first
                if secondary_button(ui, "Find").clicked() {
                    trigger_search = true;
                }
                let edit = ui.add_sized(
                    [240.0, 30.0],
                    TextEdit::singleline(&mut self.acquire_search_query)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text("Search title or AppID…"),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    trigger_search = true;
                }
            },
        );
        if trigger_search {
            let q = self.acquire_search_query.trim().to_string();
            self.execute_store_search(&q, ui.ctx());
        }

        ui.add_space(14.0);
        let body_h = (ui.available_height() - 10.0).max(280.0);
        let mut clicked_app: Option<u32> = None;

        StripBuilder::new(ui)
            .size(Size::exact(310.0))
            .size(Size::exact(16.0))
            .size(Size::remainder())
            .horizontal(|mut strip| {
                // Left: Clean Spacious Game List
                strip.cell(|ui| {
                    paper_card_padded(ui, Margin::same(10), |ui| {
                        ui.set_height(body_h - 24.0);
                        ScrollArea::vertical()
                            .id_salt("discover_list_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
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
                                    ui.add_space(24.0);
                                    ui.vertical_centered(|ui| {
                                        let empty_msg = if self.acquire_tab != AcquireTab::SearchResults
                                            && self.featured_requested
                                        {
                                            "Loading Steam catalog…"
                                        } else {
                                            "No games in this list yet"
                                        };
                                        ui.label(
                                            RichText::new(empty_msg)
                                                .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                }

                                for (aid, name) in entries {
                                    let is_sel = self.acquire_selected_app == Some(aid);
                                    if clean_list_row(ui, &name, &format!("#{}", aid), is_sel)
                                        .clicked()
                                    {
                                        clicked_app = Some(aid);
                                    }
                                }
                                ui.add_space(12.0);
                            });
                    });
                });

                strip.empty();

                // Right: Spacious Selected Game Card (scrollable so low text is never clipped)
                strip.cell(|ui| {
                    paper_card(ui, |ui| {
                        ui.set_height(body_h - 40.0);
                        ScrollArea::vertical()
                            .id_salt("discover_detail_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some(aid) = self.acquire_selected_app {
                                    let known_url = self
                                        .acquire_details
                                        .as_ref()
                                        .filter(|d| d.app_id == aid && !d.header_image.is_empty())
                                        .map(|d| d.header_image.clone())
                                        .or_else(|| self.cover_known_urls.get(&aid).cloned());
                                    self.ensure_cover_requested(aid, known_url, ui.ctx());
                                    let loading_cover = self.cover_in_flight.contains(&aid);
                                    let has_lua = self
                                        .library_rows
                                        .iter()
                                        .any(|r| r.app_id == aid && r.has_lua)
                                        || self
                                            .lua_installer
                                            .as_ref()
                                            .and_then(|i| i.read_installed_lua_path(aid))
                                            .is_some();

                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 16.0;
                                        if let Some(tex) = self.cover_textures.get(&aid) {
                                            ui.add(
                                                egui::Image::new(tex)
                                                    .fit_to_exact_size(Vec2::new(220.0, 103.0))
                                                    .corner_radius(CornerRadius::same(4)),
                                            );
                                        } else {
                                            let (rect, _) = ui.allocate_exact_size(
                                                Vec2::new(220.0, 103.0),
                                                Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                CornerRadius::same(4),
                                                Palette::BG_VOID,
                                            );
                                            let placeholder = if loading_cover {
                                                format!("Loading #{}…", aid)
                                            } else {
                                                format!("App #{}", aid)
                                            };
                                            ui.painter().text(
                                                rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                placeholder,
                                                FontId::new(13.0, FontFamily::Monospace),
                                                Palette::TEXT_MUTED,
                                            );
                                        }

                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 5.0;
                                            let title = self
                                                .acquire_details
                                                .as_ref()
                                                .filter(|d| d.app_id == aid && !d.name.is_empty())
                                                .map(|d| d.name.clone())
                                                .unwrap_or_else(|| self.resolve_game_title(aid));
                                            ui.label(
                                                RichText::new(&title)
                                                    .font(display_font(20.0))
                                                    .color(Palette::TEXT_PRIMARY),
                                            );

                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(format!("AppID {}", aid))
                                                        .font(FontId::new(
                                                            12.0,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(Palette::BLUE_LIGHT),
                                                );
                                                if has_lua {
                                                    status_badge(
                                                        ui,
                                                        "Configured",
                                                        BadgeTone::Ready,
                                                    );
                                                }
                                            });

                                            if let Some(d) = &self.acquire_details {
                                                let mut meta = Vec::new();
                                                if !d.release_date.is_empty() {
                                                    meta.push(d.release_date.clone());
                                                }
                                                if !d.genres.is_empty() {
                                                    meta.push(
                                                        d.genres
                                                            .iter()
                                                            .take(3)
                                                            .cloned()
                                                            .collect::<Vec<_>>()
                                                            .join(", "),
                                                    );
                                                }
                                                if !meta.is_empty() {
                                                    ui.label(
                                                        RichText::new(meta.join("  ·  "))
                                                            .size(12.5)
                                                            .color(Palette::TEXT_MUTED),
                                                    );
                                                }
                                            }
                                        });
                                    });

                                    ui.add_space(16.0);
                                    ui.label(
                                        RichText::new("Manifest Sources")
                                            .font(semibold_font(13.0))
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                    ui.add_space(6.0);

                                    let mut avail_providers: Vec<String> = Vec::new();
                                    inset_paper_sheet(ui, Margin::same(10), |ui| {
                                        ui.set_min_height(108.0);
                                        ui.set_width(ui.available_width());
                                        if self.acquire_sources.is_empty() {
                                            ui.label(
                                                RichText::new(&self.acquire_status)
                                                    .size(12.5)
                                                    .color(Palette::TEXT_MUTED),
                                            );
                                        } else {
                                            ui.spacing_mut().item_spacing.y = 4.0;
                                            for (prov, st) in &self.acquire_sources {
                                                let is_av = st.eq_ignore_ascii_case("available");
                                                if is_av {
                                                    avail_providers.push(prov.clone());
                                                }
                                                let is_sel = is_av
                                                    && self.acquire_selected_source.as_deref()
                                                        == Some(prov.as_str());
                                                let badge = if is_av {
                                                    "● Available"
                                                } else {
                                                    "○ Unavailable"
                                                };
                                                let row_resp =
                                                    clean_list_row(ui, prov, badge, is_sel);
                                                if is_av {
                                                    if row_resp.clicked() {
                                                        self.acquire_selected_source =
                                                            Some(prov.clone());
                                                    }
                                                } else {
                                                    row_resp.on_hover_text(
                                                        "This provider does not currently host a manifest for this AppID.",
                                                    );
                                                }
                                            }
                                        }
                                    });

                                    ui.add_space(14.0);
                                    let selected_title = self
                                        .acquire_details
                                        .as_ref()
                                        .filter(|d| d.app_id == aid && !d.name.is_empty())
                                        .map(|d| d.name.clone())
                                        .unwrap_or_else(|| self.resolve_game_title(aid));
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                                        ui.add_enabled_ui(
                                            !avail_providers.is_empty() && !self.acquire_busy,
                                            |ui| {
                                                if primary_button(ui, "Install Game")
                                                    .on_hover_text(
                                                        "Download and install the .lua manifest package from the selected provider",
                                                    )
                                                    .clicked()
                                                {
                                                    let chosen = self
                                                        .acquire_selected_source
                                                        .clone()
                                                        .filter(|s| avail_providers.contains(s))
                                                        .or_else(|| {
                                                            avail_providers.first().cloned()
                                                        });
                                                    if let Some(src) = chosen {
                                                        self.start_acquire_download(
                                                            aid,
                                                            src,
                                                            ui.ctx(),
                                                        );
                                                    }
                                                }
                                            },
                                        );

                                        if has_lua {
                                            if secondary_button(ui, "Open Setup / Integration")
                                                .on_hover_text(
                                                    "Open Integration & Process Controls (1-click Recommended Fixes, Play Unowned, AppID 480 redirect, Goldberg)",
                                                )
                                                .clicked()
                                            {
                                                self.open_integration_for_app(aid);
                                            }
                                            if secondary_button(ui, "Files & DLC")
                                                .on_hover_text(
                                                    "Open Depots & Vault to toggle DLCs, switch saved .lua builds, pin manifests, or download depots",
                                                )
                                                .clicked()
                                            {
                                                self.open_game_in_depots(aid, ui.ctx());
                                            }
                                            if secondary_button(ui, "Game Fixes")
                                                .on_hover_text(
                                                    "Browse and apply verified DRM / online patches for this game",
                                                )
                                                .clicked()
                                            {
                                                self.open_app_in_fixes(aid, ui.ctx());
                                            }
                                            if secondary_button(ui, "Launch Options")
                                                .on_hover_text(
                                                    "View or edit Steam appinfo.vdf launch executables and arguments",
                                                )
                                                .clicked()
                                            {
                                                self.open_launch_options_modal(
                                                    aid,
                                                    selected_title.clone(),
                                                );
                                            }
                                        }

                                        if self.steam_service.is_available()
                                            && !self.steam_library_service.is_game_in_library(aid)
                                            && secondary_button(ui, "+ Add to Steam Library")
                                                .on_hover_text(
                                                    "Write Valve appmanifest_<appid>.acf so this game appears in your Steam Library (close Steam first)",
                                                )
                                                .clicked()
                                        {
                                            let gname = selected_title.clone();
                                            match self.steam_library_service.add_game_to_library(
                                                aid,
                                                &gname,
                                                None,
                                                &mut self.slssteam_service,
                                            ) {
                                                Ok(_) => {
                                                    let _ = self
                                                        .play_unowned_service
                                                        .sync_plugin_file(&self.slssteam_service);
                                                    self.refresh_all_local_state();
                                                    self.notify(
                                                        format!(
                                                            "Added {} (#{}) to Steam Library",
                                                            gname, aid
                                                        ),
                                                        ToastKind::Success,
                                                    );
                                                }
                                                Err(e) => self.notify(e, ToastKind::Warning),
                                            }
                                        }

                                        if let Some(dlc) = &self.acquire_dlc_info {
                                            if dlc.missing_count > 0 {
                                                let missing = dlc.missing_count;
                                                let mut trigger_dlc = false;
                                                ui.add_enabled_ui(!self.acquire_busy, |ui| {
                                                    if secondary_button(
                                                        ui,
                                                        &format!("Generate DLC (+{})", missing),
                                                    )
                                                    .on_hover_text(
                                                        "Generate missing DLC entries into this game's stplug-in .lua file",
                                                    )
                                                    .clicked()
                                                    {
                                                        trigger_dlc = true;
                                                    }
                                                });
                                                if trigger_dlc {
                                                    self.start_dlc_generation(aid, ui.ctx());
                                                }
                                            }
                                        }

                                        if ghost_button(ui, "Steam Store ›")
                                            .on_hover_text(
                                                "Open this game's official Steam Store page in your browser",
                                            )
                                            .clicked()
                                        {
                                            let _ = Command::new("xdg-open")
                                                .arg(format!(
                                                    "https://store.steampowered.com/app/{}",
                                                    aid
                                                ))
                                                .spawn();
                                        }
                                    });

                                    if self.acquire_busy {
                                        ui.add_space(8.0);
                                        ui.add(
                                            ProgressBar::new(self.acquire_progress)
                                                .show_percentage()
                                                .corner_radius(CornerRadius::same(3)),
                                        );
                                    }
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(&self.acquire_status)
                                            .size(12.0)
                                            .color(Palette::TEXT_MUTED),
                                    );
                                    ui.add_space(12.0);
                                    egui::CollapsingHeader::new(
                                        RichText::new(format!(
                                            "Recent Downloads ({})",
                                            self.download_history.len()
                                        ))
                                        .font(semibold_font(12.5))
                                        .color(Palette::TEXT_SECONDARY),
                                    )
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        if self.download_history.is_empty() {
                                            ui.label(
                                                RichText::new("No downloads recorded in history yet.")
                                                    .size(12.0)
                                                    .color(Palette::TEXT_MUTED),
                                            );
                                        } else {
                                            for item in self.download_history.iter().take(25) {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "[{}] #{} {} ({}) — {}",
                                                        item.timestamp,
                                                        item.app_id,
                                                        item.label,
                                                        item.source,
                                                        item.status
                                                    ))
                                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                                    .color(Palette::TEXT_SECONDARY),
                                                );
                                            }
                                        }
                                    });
                                    ui.add_space(16.0);
                                } else {
                                    ui.vertical_centered(|ui| {
                                        ui.add_space(70.0);
                                        draw_rem_medallion(ui, 48.0);
                                        ui.add_space(10.0);
                                        ui.label(
                                            RichText::new("Select a game from the list")
                                                .font(display_font(17.0))
                                                .color(Palette::TEXT_PRIMARY),
                                        );
                                        ui.label(
                                            RichText::new(
                                                "Or search by name, AppID, or Steam link above.",
                                            )
                                            .size(13.0)
                                            .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                }
                            });
                    });
                });
            });

        if let Some(aid) = clicked_app {
            self.open_app_in_acquire(aid, ui.ctx());
        }
    }

    // ===== 3. LIBRARY =====

    pub fn render_library(&mut self, ui: &mut egui::Ui) {
        let mut open_add = false;
        let mut do_refresh = false;
        let mut do_bulk_del = false;

        section_header(
            ui,
            "MY GAMES",
            "My Games",
            "Click Integrate for 1-click fixes & setup, Files & DLC for depots, Edit .lua for script overrides, or More",
            |ui| {
                if ghost_button(ui, "Refresh")
                    .on_hover_text("Rescan local Steam libraries and stplug-in .lua files")
                    .clicked()
                {
                    do_refresh = true;
                }
                if !self.library_selected_ids.is_empty()
                    && danger_button(
                        ui,
                        &format!("Remove Selected ({})", self.library_selected_ids.len()),
                    )
                    .on_hover_text("Remove stplug-in .lua files for all checked games")
                    .clicked()
                {
                    do_bulk_del = true;
                }
                if primary_button(ui, "+ Add by AppID")
                    .on_hover_text("Manually create a starter stplug-in .lua file for any Steam AppID")
                    .clicked()
                {
                    open_add = true;
                }
            },
        );

        if open_add {
            self.add_appid_modal.open = true;
        }
        if do_refresh {
            self.refresh_all_local_state();
        }
        if do_bulk_del {
            if let Some(installer) = self.lua_installer.clone() {
                let ids: Vec<u32> = self.library_selected_ids.iter().copied().collect();
                let mut removed = 0usize;
                for aid in ids {
                    let res = installer.uninstall_lua(aid, true, Some(&mut self.slssteam_service));
                    if res.ok {
                        removed += 1;
                    }
                }
                self.library_selected_ids.clear();
                self.refresh_all_local_state();
                if removed > 0 {
                    self.notify(
                        format!("Removed {} .lua manifest(s).", removed),
                        ToastKind::Info,
                    );
                } else {
                    self.notify(
                        "Selected title(s) had no stplug-in .lua file to remove.",
                        ToastKind::Warning,
                    );
                }
            } else {
                self.notify("Steam installation not detected.", ToastKind::Warning);
            }
        }

        let total_n = self.library_rows.len();
        let lua_n = self.library_rows.iter().filter(|r| r.has_lua).count();
        let inst_n = self.library_rows.iter().filter(|r| r.installed).count();
        let over_n = self
            .library_rows
            .iter()
            .filter(|r| r.fake_id.is_some() || r.has_token)
            .count();

        egui::Sides::new().show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if chip_button(
                    ui,
                    &format!("All ({})", total_n),
                    self.library_filter_mode == 0,
                )
                .clicked()
                {
                    self.library_filter_mode = 0;
                }
                if chip_button(
                    ui,
                    &format!("Added ({})", lua_n),
                    self.library_filter_mode == 1,
                )
                .clicked()
                {
                    self.library_filter_mode = 1;
                }
                if chip_button(
                    ui,
                    &format!("Installed ({})", inst_n),
                    self.library_filter_mode == 2,
                )
                .clicked()
                {
                    self.library_filter_mode = 2;
                }
                if chip_button(
                    ui,
                    &format!("Custom ({})", over_n),
                    self.library_filter_mode == 3,
                )
                .clicked()
                {
                    self.library_filter_mode = 3;
                }
            },
            |ui| {
                ui.add_sized(
                    [240.0, 30.0],
                    TextEdit::singleline(&mut self.library_search)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text("Filter titles…"),
                );
            },
        );

        ui.add_space(14.0);

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
        let mut act_delete: Option<u32> = None;
        let mut act_add_lib: Option<(u32, String)> = None;
        let mut act_rem_lib: Option<u32> = None;
        let mut act_toggle_unowned: Option<(u32, bool)> = None;
        let mut act_autopatch: Option<(u32, PathBuf)> = None;
        let mut act_steamless: Option<(u32, PathBuf)> = None;
        let mut act_migrate_legacy: Option<(u32, PathBuf)> = None;
        let mut act_integration: Option<(u32, String, Option<PathBuf>)> = None;
        let mut act_launch_integrated: Option<(u32, Option<PathBuf>)> = None;

        paper_card_padded(ui, Margin::symmetric(14, 10), |ui| {
            let table_h = (ui.available_height() - 12.0).max(220.0);
            ui.set_min_height(table_h);

            if filtered.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(56.0);
                    ui.label(
                        RichText::new("No matching titles found")
                            .font(semibold_font(14.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                    ui.label(
                        RichText::new("Try changing the filter above or click + Add by AppID.")
                            .size(13.0)
                            .color(Palette::TEXT_MUTED),
                    );
                });
                return;
            }

            ui.spacing_mut().item_spacing.x = 8.0;
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(30.0))
                .column(Column::remainder().at_least(140.0))
                .column(Column::exact(128.0))
                .column(Column::exact(120.0))
                .column(Column::exact(326.0))
                .header(32.0, |mut header| {
                    header.col(|ui| {
                        let mut all_selected = !filtered.is_empty()
                            && filtered
                                .iter()
                                .all(|r| self.library_selected_ids.contains(&r.app_id));
                        if ui
                            .add(Checkbox::without_text(&mut all_selected))
                            .on_hover_text("Select / deselect all visible titles")
                            .changed()
                        {
                            if all_selected {
                                for r in &filtered {
                                    self.library_selected_ids.insert(r.app_id);
                                }
                            } else {
                                for r in &filtered {
                                    self.library_selected_ids.remove(&r.app_id);
                                }
                            }
                        }
                    });
                    for h in ["Game", "Status", "Overrides", "Actions"] {
                        header.col(|ui| {
                            ui.label(
                                RichText::new(h)
                                    .font(semibold_font(12.0))
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    }
                })
                .body(|mut body| {
                    for r in &filtered {
                        body.row(44.0, |mut row| {
                            row.col(|ui| {
                                let mut checked = self.library_selected_ids.contains(&r.app_id);
                                if ui.add(Checkbox::without_text(&mut checked)).changed() {
                                    if checked {
                                        self.library_selected_ids.insert(r.app_id);
                                    } else {
                                        self.library_selected_ids.remove(&r.app_id);
                                    }
                                }
                            });

                            row.col(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let name_resp = ui.add(
                                    egui::Label::new(
                                        RichText::new(truncate_label(&r.name, 38))
                                            .font(semibold_font(13.5))
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .sense(Sense::click()),
                                );
                                if name_resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if name_resp
                                    .on_hover_text(format!(
                                        "{} (#{}) — click to open Integration & Quick Setup",
                                        r.name, r.app_id
                                    ))
                                    .clicked()
                                {
                                    act_integration =
                                        Some((r.app_id, r.name.clone(), r.install_path.clone()));
                                }
                                ui.label(
                                    RichText::new(format!("#{}", r.app_id))
                                        .font(FontId::new(11.5, FontFamily::Monospace))
                                        .color(Palette::TEXT_MUTED),
                                );
                            });

                            row.col(|ui| {
                                let tone = match (r.installed, r.has_lua) {
                                    (true, true) => BadgeTone::Ready,
                                    (false, true) => BadgeTone::Blue,
                                    _ => BadgeTone::Neutral,
                                };
                                status_badge(ui, r.status_label, tone);
                            });

                            row.col(|ui| {
                                let mut tags = Vec::new();
                                if let Some(fid) = r.fake_id {
                                    tags.push(format!("Fake {}", fid));
                                }
                                if r.has_token {
                                    tags.push("Token".into());
                                }
                                if tags.is_empty() {
                                    ui.label(
                                        RichText::new("Standard")
                                            .size(12.0)
                                            .color(Palette::TEXT_MUTED),
                                    );
                                } else {
                                    status_badge(ui, &tags.join(" · "), BadgeTone::Blue);
                                }
                            });

                            row.col(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if table_button(ui, "Integrate", true)
                                    .on_hover_text(
                                        "Open Integration & Process Controls (1-click Recommended Fixes, Play Unowned, AppID 480 redirect, keys & LD_PRELOAD)",
                                    )
                                    .clicked()
                                {
                                    act_integration = Some((
                                        r.app_id,
                                        r.name.clone(),
                                        r.install_path.clone(),
                                    ));
                                }
                                if table_button(ui, "Files & DLC", false)
                                    .on_hover_text(
                                        "Open Depots & Vault to toggle DLCs, switch saved .lua builds, pin manifests, or download depots",
                                    )
                                    .clicked()
                                {
                                    act_depots = Some(r.app_id);
                                }
                                if table_button(ui, "Edit .lua", false)
                                    .on_hover_text(
                                        "Edit stplug-in/<appid>.lua script, FakeAppId override, and AppToken",
                                    )
                                    .clicked()
                                {
                                    act_config = Some((r.app_id, r.name.clone()));
                                }
                                ui.scope(|ui| {
                                    ui.spacing_mut().button_padding = Vec2::new(10.0, 5.0);
                                    let menu_resp = ui.menu_button("More", |ui| {
                                        ui.set_min_width(252.0);
                                        ui.spacing_mut().item_spacing.y = 4.0;

                                        ui.label(
                                            RichText::new("FIXES & LAUNCH")
                                                .font(semibold_font(10.5))
                                                .color(Palette::TEXT_MUTED),
                                        );
                                        if ui
                                            .button("Game Fixes…")
                                            .on_hover_text(
                                                "Browse and apply verified DRM / online patches with automatic SHA-256 backup",
                                            )
                                            .clicked()
                                        {
                                            act_fixes = Some(r.app_id);
                                            ui.close_menu();
                                        }
                                        if ui
                                            .button("Launch Options…")
                                            .on_hover_text(
                                                "Edit Steam appinfo.vdf launch executables, arguments, and working directory",
                                            )
                                            .clicked()
                                        {
                                            act_launch = Some((r.app_id, r.name.clone()));
                                            ui.close_menu();
                                        }
                                        if ui
                                            .button("Launch Game with Bridge")
                                            .on_hover_text(
                                                "Start the game executable directly with per-app LD_PRELOAD library integration",
                                            )
                                            .clicked()
                                        {
                                            act_launch_integrated =
                                                Some((r.app_id, r.install_path.clone()));
                                            ui.close_menu();
                                        }

                                        ui.separator();
                                        ui.label(
                                            RichText::new("STEAM & OWNERSHIP")
                                                .font(semibold_font(10.5))
                                                .color(Palette::TEXT_MUTED),
                                        );
                                        if !r.installed {
                                            if ui
                                                .button("Add Steam Library Entry")
                                                .on_hover_text(
                                                    "Write Valve appmanifest_<appid>.acf so this game appears in Steam (close Steam first)",
                                                )
                                                .clicked()
                                            {
                                                act_add_lib = Some((r.app_id, r.name.clone()));
                                                ui.close_menu();
                                            }
                                        } else if ui
                                            .button("Remove Steam Library Entry")
                                            .on_hover_text(
                                                "Remove appmanifest_<appid>.acf registration from your Steam Library (close Steam first)",
                                            )
                                            .clicked()
                                        {
                                            act_rem_lib = Some(r.app_id);
                                            ui.close_menu();
                                        }
                                        let is_unowned_on = self
                                            .play_unowned_service
                                            .is_app_enabled(r.app_id, &self.slssteam_service);
                                        let unowned_lbl = if is_unowned_on {
                                            "Disable Play Unowned"
                                        } else {
                                            "Enable Play Unowned"
                                        };
                                        if ui
                                            .button(unowned_lbl)
                                            .on_hover_text(
                                                "Toggle Play-Unowned ownership bypass in play_unowned_games.lua and SLSsteam",
                                            )
                                            .clicked()
                                        {
                                            act_toggle_unowned = Some((r.app_id, !is_unowned_on));
                                            ui.close_menu();
                                        }

                                        ui.separator();
                                        ui.label(
                                            RichText::new("LOCAL GAME FILES")
                                                .font(semibold_font(10.5))
                                                .color(Palette::TEXT_MUTED),
                                        );
                                        let has_local = r.install_path.is_some();
                                        let missing_local_tip = "Game files are not installed locally yet — download the game in Steam or Depots & Vault first.";
                                        if ui
                                            .add_enabled(
                                                has_local,
                                                egui::Button::new("Offline Patch (Goldberg)"),
                                            )
                                            .on_hover_text(
                                                "Apply native Goldberg Steam emulator to the game folder for offline play",
                                            )
                                            .on_disabled_hover_text(missing_local_tip)
                                            .clicked()
                                        {
                                            if let Some(ref p) = r.install_path {
                                                act_autopatch = Some((r.app_id, p.clone()));
                                            }
                                            ui.close_menu();
                                        }
                                        if ui
                                            .add_enabled(
                                                has_local,
                                                egui::Button::new(
                                                    "Remove SteamStub DRM (Steamless)",
                                                ),
                                            )
                                            .on_hover_text(
                                                "Unpack SteamStub-protected game .exe files using Steamless",
                                            )
                                            .on_disabled_hover_text(missing_local_tip)
                                            .clicked()
                                        {
                                            if let Some(ref p) = r.install_path {
                                                act_steamless = Some((r.app_id, p.clone()));
                                            }
                                            ui.close_menu();
                                        }
                                        if ui
                                            .add_enabled(
                                                has_local,
                                                egui::Button::new("Clean Old Unlockers"),
                                            )
                                            .on_hover_text(
                                                "Remove leftover SmokeAPI, GreenLuma, or Koalageddon DLLs and restore original files",
                                            )
                                            .on_disabled_hover_text(missing_local_tip)
                                            .clicked()
                                        {
                                            if let Some(ref p) = r.install_path {
                                                act_migrate_legacy = Some((r.app_id, p.clone()));
                                            }
                                            ui.close_menu();
                                        }
                                        if ui
                                            .add_enabled(
                                                has_local,
                                                egui::Button::new("Open Game Folder"),
                                            )
                                            .on_hover_text(
                                                "Open the local game installation folder in your file manager",
                                            )
                                            .on_disabled_hover_text(missing_local_tip)
                                            .clicked()
                                        {
                                            if let Some(ref p) = r.install_path {
                                                let _ = Command::new("xdg-open").arg(p).spawn();
                                            }
                                            ui.close_menu();
                                        }

                                        if r.has_lua {
                                            ui.separator();
                                            if ui
                                                .button(
                                                    RichText::new("Remove from LuaTools")
                                                        .color(Palette::DANGER_TEXT),
                                                )
                                                .on_hover_text(
                                                    "Delete stplug-in/<appid>.lua and clear this game's unlocker overrides",
                                                )
                                                .clicked()
                                            {
                                                act_delete = Some(r.app_id);
                                                ui.close_menu();
                                            }
                                        }
                                    });
                                    menu_resp.response.on_hover_text(
                                        "More per-game tools: Game Fixes, Launch Options, Steam Library Entry, Play Unowned, Goldberg, Steamless, and folder actions",
                                    );
                                });
                            });
                        });
                    }
                });
        });

        if let Some((aid, gname)) = act_config {
            self.open_game_detail_modal(aid, gname);
        }
        if let Some(aid) = act_depots {
            self.open_game_in_depots(aid, ui.ctx());
        }
        if let Some((aid, gname)) = act_launch {
            self.open_launch_options_modal(aid, gname);
        }
        if let Some(aid) = act_fixes {
            self.open_app_in_fixes(aid, ui.ctx());
        }
        if let Some(aid) = act_delete {
            if let Some(installer) = self.lua_installer.clone() {
                let res = installer.uninstall_lua(aid, true, Some(&mut self.slssteam_service));
                self.refresh_all_local_state();
                if res.ok {
                    self.notify(format!("Removed {}.lua", aid), ToastKind::Info);
                } else {
                    self.notify(
                        res.error
                            .unwrap_or_else(|| format!("Failed to remove {}.lua", aid)),
                        ToastKind::Warning,
                    );
                }
            } else {
                self.notify("Steam installation not detected.", ToastKind::Warning);
            }
        }
        if let Some((aid, gname)) = act_add_lib {
            match self.steam_library_service.add_game_to_library(
                aid,
                &gname,
                None,
                &mut self.slssteam_service,
            ) {
                Ok(_) => {
                    let _ = self
                        .play_unowned_service
                        .sync_plugin_file(&self.slssteam_service);
                    self.refresh_all_local_state();
                    self.notify(
                        format!("Registered {} (#{}) in Steam Library", gname, aid),
                        ToastKind::Success,
                    );
                }
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if let Some(aid) = act_rem_lib {
            match self
                .steam_library_service
                .remove_game_from_library(aid, &mut self.slssteam_service)
            {
                Ok(()) => {
                    let _ = self
                        .play_unowned_service
                        .sync_plugin_file(&self.slssteam_service);
                    self.refresh_all_local_state();
                    self.notify(
                        format!("Removed AppID {} ACF registration from Steam Library", aid),
                        ToastKind::Info,
                    );
                }
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if let Some((aid, enable)) = act_toggle_unowned {
            let res = if enable {
                self.play_unowned_service
                    .enable_for_app(aid, &mut self.slssteam_service)
            } else {
                self.play_unowned_service
                    .disable_for_app(aid, &mut self.slssteam_service)
            };
            match res {
                Ok(()) => {
                    self.refresh_all_local_state();
                    self.notify(
                        if enable {
                            format!("Enabled Play Unowned for AppID {}", aid)
                        } else {
                            format!("Disabled Play Unowned for AppID {}", aid)
                        },
                        ToastKind::Success,
                    );
                }
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if let Some((aid, dir)) = act_autopatch {
            match self.steam_auto_crack.launch_for_app(aid, &dir) {
                Ok(m) => self.notify(m, ToastKind::Info),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if let Some((aid, dir)) = act_steamless {
            self.run_steamless_on_game_async(aid, dir, ui.ctx());
        }
        if let Some((aid, dir)) = act_migrate_legacy {
            let report =
                crate::services::mode_migration::ModeMigration::migrate_legacy_unlockers_in_game(
                    &dir,
                );
            if !report.errors.is_empty() {
                self.notify(
                    format!(
                        "Legacy unlocker cleanup for AppID {} had errors: {}",
                        aid,
                        report.errors.join("; ")
                    ),
                    ToastKind::Warning,
                );
            } else if report.detected_files.is_empty() {
                self.notify(
                    format!("No legacy unlocker files found in AppID {}.", aid),
                    ToastKind::Info,
                );
            } else {
                self.notify(
                    format!(
                        "Cleaned {} legacy unlocker file(s) and restored {} original DLL(s) for AppID {}.",
                        report.removed_files.len(),
                        report.restored_files.len(),
                        aid
                    ),
                    ToastKind::Success,
                );
            }
        }
        if let Some((aid, gname, path)) = act_integration {
            self.open_integration_modal(aid, gname, path);
        }
        if let Some((aid, path)) = act_launch_integrated {
            match self
                .game_injection_service
                .launch_with_integration(aid, path.as_deref())
            {
                Ok(m) => self.notify(m, ToastKind::Success),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
    }

    pub fn open_game_detail_modal(&mut self, app_id: u32, game_name: String) {
        let lua_text = self
            .lua_installer
            .as_ref()
            .and_then(|i| i.read_lua_text(app_id))
            .unwrap_or_default();
        let fake_id = self
            .slssteam_service
            .get_fake_app_id(app_id)
            .map(|x| x.to_string())
            .unwrap_or_default();
        let token = self
            .slssteam_service
            .get_token(app_id)
            .unwrap_or("")
            .to_string();

        self.game_detail_modal.open = true;
        self.game_detail_modal.app_id = app_id;
        self.game_detail_modal.game_name = game_name;
        self.game_detail_modal.lua_text = lua_text;
        self.game_detail_modal.fake_id_input = fake_id;
        self.game_detail_modal.token_input = token;
        self.game_detail_modal.show_token = false;
        self.game_detail_modal.quick_depot_input.clear();
        self.game_detail_modal.feedback.clear();
    }

    pub fn open_launch_options_modal(&mut self, app_id: u32, game_name: String) {
        let entries = self
            .steam_service
            .installation
            .as_ref()
            .map(|i| LaunchOptionsService::new(i.root.clone()).get_launch_entries(app_id))
            .unwrap_or_default();
        self.launch_options_modal.selected_idx = (!entries.is_empty()).then_some(0);
        self.launch_options_modal.app_id = app_id;
        self.launch_options_modal.game_name = game_name;
        self.launch_options_modal.entries = entries;
        self.launch_options_modal.feedback.clear();
        self.launch_options_modal.steam_running =
            crate::services::launch_mod_store::is_any_steam_running();
        self.launch_options_modal.busy = false;
        self.launch_options_modal.open = true;
    }
}

fn summary_paper_tile(
    ui: &mut egui::Ui,
    eyebrow: &str,
    headline: &str,
    sub: &str,
    badge: &str,
    tone: BadgeTone,
) -> egui::Response {
    let inner = paper_card_padded(ui, Margin::symmetric(16, 14), |ui| {
        ui.set_min_height(78.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new(eyebrow)
                            .font(semibold_font(10.5))
                            .color(Palette::TEXT_MUTED),
                    );
                },
                |ui| {
                    status_badge(ui, badge, tone);
                },
            );
            ui.label(
                RichText::new(headline)
                    .font(display_font(16.0))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(RichText::new(sub).size(12.0).color(Palette::TEXT_SECONDARY));
        });
    });
    let resp = ui.interact(
        inner.response.rect,
        ui.id().with(("summary_tile", eyebrow)),
        Sense::click(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub fn clean_list_row(
    ui: &mut egui::Ui,
    title: &str,
    right_meta: &str,
    selected: bool,
) -> egui::Response {
    let w = ui.available_width();
    let desired = Vec2::new(w, 34.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let r = CornerRadius::same(4);
        if selected {
            p.rect_filled(rect, r, Palette::BLUE_SUBTLE);
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + 5.0),
                Vec2::new(3.0, rect.height() - 10.0),
            );
            p.rect_filled(bar, CornerRadius::same(2), Palette::BLUE_LIGHT);
        } else if resp.hovered() {
            p.rect_filled(rect, r, Palette::BG_ELEVATED);
        }

        let fg = if selected {
            Palette::TEXT_PRIMARY
        } else {
            Palette::TEXT_SECONDARY
        };
        p.text(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            truncate_label(title, 28),
            semibold_font(12.8),
            fg,
        );
        p.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            right_meta,
            FontId::new(11.0, FontFamily::Monospace),
            if selected {
                Palette::BLUE_LIGHT
            } else {
                Palette::TEXT_MUTED
            },
        );
    }
    if title.chars().count() > 28 {
        resp.on_hover_text(title)
    } else {
        resp
    }
}

fn truncate_label(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
