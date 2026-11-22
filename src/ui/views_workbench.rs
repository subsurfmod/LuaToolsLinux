//! Workbench Views: `04 // Depots & Vault`, `05 // Fix Catalog`, `06 // Toolchain`, and `07 // System`.

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use eframe::egui::{
    self, Align, ComboBox, Frame, Layout, Margin, ProgressBar, RichText, ScrollArea, Stroke,
    TextEdit,
};
use egui_extras::{Column, TableBuilder};

use crate::parsers::lua_parser::LuaFileParser;
use crate::services::api_client::HubcapService;
use crate::services::cef_injector::CefInjectorService;
use crate::services::depot_downloader::DepotSelection;
use crate::services::external_tools::TOOLS;
use crate::services::fixes_service::FixesService;
use crate::services::protocol_service::ProtocolService;
use crate::ui::app::LuaToolsApp;
use crate::ui::state::{AppMessage, DepotsSubTab};
use crate::ui::theme::{
    chip_button, danger_button, format_bytes, ghost_button, lua_layouter, panel_frame,
    pick_directory_native, pick_files_native, primary_button, secondary_button, section_header,
    status_badge, table_button, BadgeTone, Palette,
};

impl LuaToolsApp {
    // ===== 04 // DEPOTS & VAULT =====

    pub fn render_depots(&mut self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "04",
            "DEPOTS & LUAVAULT WORKBENCH",
            "Switch stored manifest builds, edit live stplug-in .lua with syntax highlighting, toggle pins, and download depots",
            |_ui| {},
        );
        ui.add_space(6.0);

        // Active Title Selector Bar
        let mut switch_to_app: Option<u32> = None;
        let mut do_refresh = false;

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("ACTIVE TITLE:")
                    .monospace()
                    .strong()
                    .size(11.0)
                    .color(Palette::TEXT_MUTED),
            );

            let current_lbl = self
                .depots_app_id
                .map(|aid| format!("{}  ({})", self.resolve_game_title(aid), aid))
                .unwrap_or_else(|| "Select a game...".into());

            ComboBox::from_id_salt("depots_game_combo")
                .width(310.0)
                .selected_text(current_lbl)
                .show_ui(ui, |ui| {
                    for r in &self.library_rows {
                        let tag = if r.has_lua { "LUA" } else { "STEAM" };
                        let txt = format!("[{}]  {}  ({})", tag, r.name, r.app_id);
                        if ui
                            .selectable_label(self.depots_app_id == Some(r.app_id), txt)
                            .clicked()
                        {
                            switch_to_app = Some(r.app_id);
                        }
                    }
                });

            let jump_resp = ui.add(
                TextEdit::singleline(&mut self.depots_jump_input)
                    .hint_text("Or AppID...")
                    .desired_width(110.0),
            );
            let enter = jump_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || secondary_button(ui, "LOAD").clicked() {
                if let Ok(aid) = self.depots_jump_input.trim().parse::<u32>() {
                    if aid > 0 {
                        self.depots_jump_input.clear();
                        switch_to_app = Some(aid);
                    }
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let picker_active = self.depots_subtab == DepotsSubTab::DownloadPicker;
                let toggle_txt = if picker_active {
                    "EXIT DOWNLOAD PICKER"
                } else {
                    "DOWNLOAD DEPOTS"
                };
                if primary_button(ui, toggle_txt).clicked() && self.depots_app_id.is_some() {
                    if picker_active {
                        self.depots_subtab = DepotsSubTab::Declarations;
                    } else {
                        self.depots_subtab = DepotsSubTab::DownloadPicker;
                        self.rebuild_depot_picks();
                    }
                }
                if ghost_button(ui, "REFRESH").clicked() {
                    do_refresh = true;
                }
                if let Some(aid) = self.depots_app_id {
                    if let Some(info) = &self.depots_info {
                        if let Some(bid) = &info.public_build_id {
                            ui.label(
                                RichText::new(format!("BUILD {}", bid))
                                    .monospace()
                                    .size(11.0)
                                    .color(Palette::BLUE_LIGHT),
                            );
                        }
                    }
                    ui.label(
                        RichText::new(format!("APPID // {}", aid))
                            .monospace()
                            .strong()
                            .color(Palette::BLUE_LIGHT),
                    );
                }
            });
        });

        if let Some(aid) = switch_to_app {
            self.open_game_in_depots(aid, ui.ctx());
        }
        if do_refresh {
            if let Some(aid) = self.depots_app_id {
                self.depot_info_service.invalidate(aid);
                self.open_game_in_depots(aid, ui.ctx());
            }
        }

        let avail_h = (ui.available_height() - 18.0).max(320.0);
        let active_hash = self
            .depots_app_id
            .and_then(|aid| self.lua_vault.get_active_hash(aid));

        ui.columns(2, |cols| {
            // LEFT PANE: Stored Vault Variants + Live Lua Workbench
            cols[0].set_height(avail_h);

            panel_frame().show(&mut cols[0], |ui| {
                ui.label(
                    RichText::new("STORED VAULT VARIANTS")
                        .monospace()
                        .strong()
                        .size(11.0)
                        .color(Palette::TEXT_SECONDARY),
                );
                let variants_snapshot = self.depots_variants.clone();
                let mut ctx_apply: Option<String> = None;
                let mut ctx_rename: Option<(String, String)> = None;
                let mut ctx_delete: Option<String> = None;

                ScrollArea::vertical()
                    .id_salt("vault_variants_scroll")
                    .max_height(145.0)
                    .show(ui, |ui| {
                        if variants_snapshot.is_empty() {
                            ui.label(
                                RichText::new("No stored variants for this AppID yet.")
                                    .color(Palette::TEXT_MUTED),
                            );
                        }
                        for v in &variants_snapshot {
                            let is_active = active_hash.as_deref() == Some(v.hash.as_str());
                            let is_sel = self.depots_selected_variant_hash.as_deref()
                                == Some(v.hash.as_str());
                            let marker = if is_active { "●" } else { " " };
                            let pin_tag = if v.is_pinned() { " [PIN]" } else { "" };
                            let line = format!(
                                "{} {:<22} {:<7} {:<10} {}D/{}C{}",
                                marker,
                                v.display_label(),
                                v.kind.to_ascii_uppercase(),
                                v.build_id.as_deref().unwrap_or("—"),
                                v.depot_count,
                                v.dlc_count,
                                pin_tag
                            );
                            let color = if is_active {
                                Palette::BLUE_LIGHT
                            } else {
                                Palette::TEXT_PRIMARY
                            };
                            let resp = ui.selectable_label(
                                is_sel,
                                RichText::new(line).monospace().size(11.5).color(color),
                            );
                            if resp.clicked() {
                                self.depots_selected_variant_hash = Some(v.hash.clone());
                                if !self.depots_editing_live {
                                    if let Some(aid) = self.depots_app_id {
                                        self.depots_editor_text =
                                            self.lua_vault.read_text(aid, &v.hash);
                                    }
                                }
                            }
                            resp.context_menu(|ui| {
                                if !is_active && ui.button("Apply Variant").clicked() {
                                    ctx_apply = Some(v.hash.clone());
                                    ui.close_menu();
                                }
                                if ui.button("Rename Variant...").clicked() {
                                    ctx_rename = Some((v.hash.clone(), v.display_label()));
                                    ui.close_menu();
                                }
                                if !is_active && ui.button("Delete Variant").clicked() {
                                    ctx_delete = Some(v.hash.clone());
                                    ui.close_menu();
                                }
                            });
                        }
                    });

                ui.horizontal(|ui| {
                    let sel_hash = self.depots_selected_variant_hash.clone();
                    let can_apply_or_del = sel_hash
                        .as_deref()
                        .map(|h| active_hash.as_deref() != Some(h))
                        .unwrap_or(false);

                    ui.add_enabled_ui(can_apply_or_del, |ui| {
                        if primary_button(ui, "APPLY VARIANT").clicked() {
                            ctx_apply = sel_hash.clone();
                        }
                    });
                    if secondary_button(ui, "SAVE LIVE AS PRESET").clicked()
                        && self.depots_app_id.is_some()
                    {
                        self.text_prompt_modal.open = true;
                        self.text_prompt_modal.kind = "preset".into();
                        self.text_prompt_modal.title = "SAVE LUA PRESET".into();
                        self.text_prompt_modal.prompt =
                            "Optional preset label (leave blank for timestamp):".into();
                        self.text_prompt_modal.value.clear();
                    }
                    ui.add_enabled_ui(can_apply_or_del, |ui| {
                        if danger_button(ui, "DELETE").clicked() {
                            ctx_delete = sel_hash;
                        }
                    });
                });

                if let (Some(aid), Some(h)) = (self.depots_app_id, ctx_apply) {
                    if self.lua_vault.apply(aid, &h) {
                        let live = self.lua_vault.read_live_text(aid);
                        if let Some(installer) = self.lua_installer.clone() {
                            let _ = installer.install_lua(
                                aid,
                                &live,
                                true,
                                None,
                                None,
                                Some(&mut self.settings_service),
                                Some(&mut self.slssteam_service),
                            );
                        }
                        self.reload_depots_local(aid);
                        self.status_text = "Applied stored variant to live stplug-in.".into();
                    }
                }
                if let Some((h, current_lbl)) = ctx_rename {
                    self.text_prompt_modal.open = true;
                    self.text_prompt_modal.kind = "rename_variant".into();
                    self.text_prompt_modal.title = "RENAME VARIANT".into();
                    self.text_prompt_modal.prompt = "Enter new variant label:".into();
                    self.text_prompt_modal.value = current_lbl;
                    self.text_prompt_modal.target_hash = h;
                }
                if let (Some(aid), Some(h)) = (self.depots_app_id, ctx_delete) {
                    if self.lua_vault.delete(aid, &h) {
                        self.depots_variants = self.lua_vault.get_variants(aid);
                        self.depots_selected_variant_hash = None;
                        self.status_text = "Deleted variant from LuaVault.".into();
                    }
                }
            });

            cols[0].add_space(8.0);

            // Live Lua Editor
            panel_frame().show(&mut cols[0], |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("LIVE STPLUG-IN LUA WORKBENCH")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mode_lbl = if self.depots_editing_live {
                            RichText::new("EDITING LIVE")
                                .monospace()
                                .size(10.5)
                                .color(Palette::BLUE_LIGHT)
                        } else {
                            RichText::new("READ-ONLY")
                                .monospace()
                                .size(10.5)
                                .color(Palette::TEXT_MUTED)
                        };
                        ui.label(mode_lbl);
                    });
                });

                let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                    lua_layouter(ui, string, wrap_width)
                };
                let editor_h = (ui.available_height() - 48.0).max(120.0);
                ScrollArea::vertical()
                    .id_salt("depots_lua_scroll")
                    .max_height(editor_h)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut self.depots_editor_text)
                                .code_editor()
                                .interactive(self.depots_editing_live)
                                .desired_rows(11)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter),
                        );
                    });

                ui.horizontal(|ui| {
                    if !self.depots_editing_live {
                        ui.add_enabled_ui(self.depots_app_id.is_some(), |ui| {
                            if secondary_button(ui, "EDIT LIVE LUA").clicked() {
                                if let Some(aid) = self.depots_app_id {
                                    let mut live = self.lua_vault.read_live_text(aid);
                                    if live.trim().is_empty() {
                                        live = LuaFileParser::default_lua_template(aid, "");
                                    }
                                    self.depots_editor_text = live;
                                    self.depots_editing_live = true;
                                }
                            }
                        });
                    } else {
                        if primary_button(ui, "SAVE LIVE LUA").clicked() {
                            if let Some(aid) = self.depots_app_id {
                                let txt = self.depots_editor_text.clone();
                                if let Some(installer) = self.lua_installer.clone() {
                                    let _ = installer.install_lua(
                                        aid,
                                        &txt,
                                        true,
                                        None,
                                        Some("live-edit"),
                                        Some(&mut self.settings_service),
                                        Some(&mut self.slssteam_service),
                                    );
                                } else {
                                    let _ = self.lua_vault.write_live(aid, &txt);
                                }
                                self.depots_editing_live = false;
                                self.reload_depots_local(aid);
                                self.refresh_all_local_state();
                                self.status_text = format!("Saved live {}.lua", aid);
                            }
                        }
                        if ghost_button(ui, "CANCEL").clicked() {
                            self.depots_editing_live = false;
                            if let Some(aid) = self.depots_app_id {
                                self.depots_editor_text = self.lua_vault.read_live_text(aid);
                            }
                        }
                    }
                });
            });

            // RIGHT PANE: Tabs (Depot Declarations | Download Picker)
            cols[1].set_height(avail_h);
            cols[1].horizontal(|ui| {
                if chip_button(
                    ui,
                    "DEPOT DECLARATIONS",
                    self.depots_subtab == DepotsSubTab::Declarations,
                )
                .clicked()
                {
                    self.depots_subtab = DepotsSubTab::Declarations;
                }
                if chip_button(
                    ui,
                    "DOWNLOAD PICKER",
                    self.depots_subtab == DepotsSubTab::DownloadPicker,
                )
                .clicked()
                {
                    self.depots_subtab = DepotsSubTab::DownloadPicker;
                    self.rebuild_depot_picks();
                }
            });

            match self.depots_subtab {
                DepotsSubTab::Declarations => self.render_depot_declarations_tab(&mut cols[1]),
                DepotsSubTab::DownloadPicker => self.render_depot_picker_tab(&mut cols[1]),
            }
        });
    }

    fn render_depot_declarations_tab(&mut self, ui: &mut egui::Ui) {
        panel_frame().show(ui, |ui| {
            let Some(aid) = self.depots_app_id else {
                ui.label(
                    RichText::new("Select a game above to inspect declared depots.")
                        .color(Palette::TEXT_MUTED),
                );
                return;
            };
            let Some(lua) = self.depots_lua.clone() else {
                ui.label(
                    RichText::new(
                        "No live .lua file found for this AppID. Click EDIT LIVE LUA on the left to create one.",
                    )
                    .color(Palette::TEXT_MUTED),
                );
                return;
            };

            ui.label(
                RichText::new(format!(
                    "DECLARED: {} KEYED DEPOT(S)  ·  {} DLC ENTRY(S)  ·  {} ACTIVE PIN(S)",
                    lua.depot_count(),
                    lua.dlc_count(),
                    lua.active_pins.len()
                ))
                .monospace()
                .size(11.0)
                .color(Palette::TEXT_SECONDARY),
            );
            ui.add_space(4.0);

            let depot_map: std::collections::HashMap<u32, _> = self
                .depots_info
                .as_ref()
                .map(|i| i.depots.iter().map(|d| (d.id, d.clone())).collect())
                .unwrap_or_default();

            let rows: Vec<_> = lua
                .entries
                .iter()
                .filter(|e| e.has_key() || e.id != lua.base_app_id)
                .map(|e| (e.clone(), depot_map.get(&e.id).cloned()))
                .collect();

            let mut toggle_target: Option<(u32, Option<String>)> = None;
            let mut inspect_target = None;

            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(76.0))
                .column(Column::remainder().at_least(110.0))
                .column(Column::exact(74.0))
                .column(Column::exact(68.0))
                .column(Column::exact(54.0))
                .column(Column::exact(126.0))
                .column(Column::exact(128.0))
                .header(26.0, |mut h| {
                    for lbl in [
                        "DEPOT ID",
                        "TITLE",
                        "SIZE",
                        "PLATFORM",
                        "TYPE",
                        "PIN STATUS",
                        "ACTIONS",
                    ] {
                        h.col(|ui| {
                            ui.label(
                                RichText::new(lbl)
                                    .monospace()
                                    .strong()
                                    .size(10.5)
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    }
                })
                .body(|mut body| {
                    for (entry, depot) in &rows {
                        body.row(34.0, |mut row| {
                            row.col(|ui| {
                                ui.label(RichText::new(entry.id.to_string()).monospace());
                            });
                            row.col(|ui| {
                                let title = entry.comment.clone().unwrap_or_else(|| {
                                    if !entry.has_key() {
                                        format!("DLC {}", entry.id)
                                    } else if let Some(d) = depot {
                                        if d.is_shared {
                                            "Shared".into()
                                        } else if d.is_dlc() {
                                            "DLC Depot".into()
                                        } else {
                                            "Game Content".into()
                                        }
                                    } else {
                                        format!("Depot {}", entry.id)
                                    }
                                });
                                ui.label(title);
                            });
                            row.col(|ui| {
                                let sz = entry
                                    .size_on_disk
                                    .or_else(|| depot.as_ref().map(|d| d.size))
                                    .unwrap_or(0);
                                ui.label(RichText::new(format_bytes(sz)).monospace().size(11.0));
                            });
                            row.col(|ui| {
                                let plat = depot
                                    .as_ref()
                                    .and_then(|d| d.os.as_deref())
                                    .unwrap_or("—")
                                    .replace("windows", "win");
                                ui.label(RichText::new(plat).monospace().size(11.0));
                            });
                            row.col(|ui| {
                                let t = if !entry.has_key() {
                                    "DLC"
                                } else if depot.as_ref().map(|d| d.is_shared).unwrap_or(false) {
                                    "SHARED"
                                } else {
                                    "GAME"
                                };
                                ui.label(RichText::new(t).monospace().size(11.0));
                            });
                            row.col(|ui| {
                                let (txt, col) = if let Some(gid) = &entry.manifest_id {
                                    let short = gid.get(..8).unwrap_or(gid);
                                    (format!("PIN · {}…", short), Palette::BLUE_LIGHT)
                                } else if let Some(cgid) = &entry.commented_manifest_id {
                                    let short = cgid.get(..8).unwrap_or(cgid);
                                    (format!("AUTO · {}…", short), Palette::TEXT_SECONDARY)
                                } else {
                                    ("—".into(), Palette::TEXT_MUTED)
                                };
                                ui.label(RichText::new(txt).monospace().size(11.0).color(col));
                            });
                            row.col(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if entry.has_key() {
                                    let fallback = depot
                                        .as_ref()
                                        .and_then(|d| d.public_manifest_id.clone());
                                    let can_pin = entry.manifest_id.is_some()
                                        || entry.commented_manifest_id.is_some()
                                        || fallback.is_some();
                                    ui.add_enabled_ui(can_pin, |ui| {
                                        let lbl = if entry.manifest_id.is_some() {
                                            "UNPIN"
                                        } else {
                                            "PIN"
                                        };
                                        if table_button(ui, lbl, entry.manifest_id.is_none())
                                            .clicked()
                                        {
                                            toggle_target = Some((entry.id, fallback));
                                        }
                                    });
                                }
                                if table_button(ui, "INFO", false).clicked() {
                                    inspect_target = Some((depot.clone(), entry.clone()));
                                }
                            });
                        });
                    }
                });

            if let Some((depot_id, fallback)) = toggle_target {
                let raw = self.lua_vault.read_live_text(aid);
                let (updated, changed) =
                    LuaFileParser::toggle_manifest_pin_in_text(&raw, depot_id, fallback.as_deref());
                if changed && self.lua_vault.write_live(aid, &updated) {
                    self.reload_depots_local(aid);
                    self.status_text = format!("Toggled manifest pin on depot {}", depot_id);
                }
            }
            if let Some((d, e)) = inspect_target {
                self.depot_detail_modal.open = true;
                self.depot_detail_modal.depot = d;
                self.depot_detail_modal.entry = Some(e);
            }
        });
    }

    fn render_depot_picker_tab(&mut self, ui: &mut egui::Ui) {
        panel_frame().show(ui, |ui| {
            ui.label(
                RichText::new("DOWNLOAD DESTINATION")
                    .monospace()
                    .strong()
                    .size(11.0)
                    .color(Palette::TEXT_SECONDARY),
            );
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.depots_dest_dir)
                        .desired_width(ui.available_width() - 105.0),
                );
                if secondary_button(ui, "BROWSE...").clicked() {
                    let tx = self.msg_tx.clone();
                    let ctx = ui.ctx().clone();
                    thread::spawn(move || {
                        if let Some(dir) = pick_directory_native("Select Depot Download Folder") {
                            let _ = tx.send(AppMessage::FolderPickedForDepotDest(dir));
                            ctx.request_repaint();
                        }
                    });
                }
            });

            ui.add_space(6.0);
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(42.0))
                .column(Column::exact(80.0))
                .column(Column::remainder().at_least(110.0))
                .column(Column::exact(78.0))
                .column(Column::exact(175.0))
                .header(26.0, |mut h| {
                    for lbl in ["PICK", "DEPOT ID", "TITLE", "SIZE", "STATUS"] {
                        h.col(|ui| {
                            ui.label(
                                RichText::new(lbl)
                                    .monospace()
                                    .strong()
                                    .size(10.5)
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    }
                })
                .body(|mut body| {
                    for pick in &mut self.depots_picks {
                        body.row(32.0, |mut row| {
                            row.col(|ui| {
                                ui.add_enabled_ui(pick.blocked_reason.is_none(), |ui| {
                                    ui.checkbox(&mut pick.is_selected, "");
                                });
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(pick.depot.id.to_string()).monospace());
                            });
                            row.col(|ui| {
                                let title = pick
                                    .lua_entry
                                    .as_ref()
                                    .and_then(|e| e.comment.clone())
                                    .unwrap_or_else(|| "Game Content".into());
                                ui.label(title);
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format_bytes(pick.size)).monospace());
                            });
                            row.col(|ui| {
                                let (st, col) = if let Some(b) = &pick.blocked_reason {
                                    (format!("BLOCKED: {}", b), Palette::DANGER_TEXT)
                                } else if pick.manifest_path.is_some() {
                                    ("READY (LOCAL MF)".into(), Palette::BLUE_LIGHT)
                                } else {
                                    ("READY (CDN)".into(), Palette::TEXT_PRIMARY)
                                };
                                ui.label(RichText::new(st).monospace().size(11.0).color(col));
                            });
                        });
                    }
                });

            ui.add_space(8.0);
            let sel_count = self.depots_picks.iter().filter(|p| p.is_selected).count();
            ui.horizontal(|ui| {
                if secondary_button(ui, "SELECT ALL").clicked() {
                    for p in &mut self.depots_picks {
                        if p.blocked_reason.is_none() {
                            p.is_selected = true;
                        }
                    }
                }
                if secondary_button(ui, "DESELECT ALL").clicked() {
                    for p in &mut self.depots_picks {
                        p.is_selected = false;
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_enabled_ui(sel_count > 0 && !self.depots_downloading, |ui| {
                        if primary_button(ui, &format!("DOWNLOAD SELECTED ({})", sel_count))
                            .clicked()
                        {
                            self.start_selected_depot_downloads(ui.ctx());
                        }
                    });
                });
            });

            if self.depots_downloading {
                ui.add(
                    ProgressBar::new(self.depots_dl_pct / 100.0)
                        .show_percentage()
                        .corner_radius(egui::CornerRadius::ZERO),
                );
            }
            if !self.depots_dl_status.is_empty() {
                ui.label(
                    RichText::new(&self.depots_dl_status)
                        .monospace()
                        .size(11.0)
                        .color(Palette::TEXT_SECONDARY),
                );
            }
        });
    }

    pub fn start_selected_depot_downloads(&mut self, ctx: &egui::Context) {
        let Some(app_id) = self.depots_app_id else {
            return;
        };
        let selections: Vec<DepotSelection> = self
            .depots_picks
            .iter()
            .filter(|p| p.is_selected)
            .map(|p| DepotSelection {
                depot_id: p.depot.id,
                manifest_id: p.manifest_id.clone(),
                manifest_path: p.manifest_path.clone(),
                size: p.size,
                from_app_id: p.depot.from_app_id,
                language: p.depot.language.clone(),
                os: p.depot.os.clone(),
                is_shared: p.depot.is_shared,
                is_dlc: p.depot.is_dlc(),
            })
            .collect();

        if selections.is_empty() {
            return;
        }

        let dest_root = PathBuf::from(self.depots_dest_dir.trim());
        let keys = crate::services::depot_downloader::DepotDownloaderService::resolve_keys(
            self.steam_service.installation.as_ref(),
            &self.slssteam_service,
            app_id,
        );
        let wp = self.slssteam_service.config.workshop_path.clone();
        let dd = self.depot_downloader.clone();
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        self.depots_downloading = true;
        self.depots_dl_pct = 0.0;
        self.depots_dl_status = "Starting DepotDownloader...".into();

        thread::spawn(move || {
            let mut results = Vec::new();
            for sel in selections {
                let sub = dest_root.join(format!("depot_{}", sel.depot_id));
                let tx_p = tx.clone();
                let ctx_p = ctx_c.clone();
                let label = format!("Depot {}", sel.depot_id);
                let r = dd.download_depot(app_id, &sel, &sub, &keys, Some(&wp), move |pct, msg| {
                    let _ = tx_p.send(AppMessage::DepotDownloadProgress {
                        depot_label: label.clone(),
                        pct,
                        msg,
                    });
                    ctx_p.request_repaint();
                });
                results.push(r);
            }
            let _ = tx.send(AppMessage::DepotDownloadFinished { results });
            ctx_c.request_repaint();
        });
    }

    // ===== 05 // FIX CATALOG =====

    pub fn render_fixes(&mut self, ui: &mut egui::Ui) {
        let mut do_refresh = false;
        section_header(
            ui,
            "05",
            "GAME & DENUVO FIX CATALOG",
            "Verified manifests and patches with automatic .luatools-fix SHA-256 backup and clean revert",
            |ui| {
                if primary_button(ui, "REFRESH CATALOG").clicked() {
                    do_refresh = true;
                }
            },
        );
        if do_refresh {
            self.fetch_fix_listings_async(ui.ctx());
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.fixes_search)
                    .hint_text("Search fix catalog by game title or AppID...")
                    .desired_width(ui.available_width() - 240.0),
            );
            ui.checkbox(&mut self.fixes_my_games_only, "MY CONFIGURED GAMES ONLY");
        });

        // Tag Filter Chips
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("TAGS:")
                    .monospace()
                    .strong()
                    .size(10.5)
                    .color(Palette::TEXT_MUTED),
            );
            if chip_button(ui, "ALL", self.fixes_selected_tag.is_none()).clicked() {
                self.fixes_selected_tag = None;
            }
            let tags = self.fixes_tags.clone();
            for t in tags {
                let active = self.fixes_selected_tag.as_deref() == Some(t.id.as_str());
                if chip_button(ui, &t.name.to_ascii_uppercase(), active).clicked() {
                    if active {
                        self.fixes_selected_tag = None;
                    } else {
                        self.fixes_selected_tag = Some(t.id);
                    }
                }
            }
        });

        let my_ids: std::collections::HashSet<String> = self
            .lua_installer
            .as_ref()
            .map(|i| {
                i.get_installed_games()
                    .into_iter()
                    .map(|x| x.to_string())
                    .collect()
            })
            .unwrap_or_default();

        let q = self.fixes_search.trim().to_ascii_lowercase();
        let filtered_games: Vec<_> = self
            .fixes_listings
            .iter()
            .filter(|g| {
                if let Some(tid) = &self.fixes_selected_tag {
                    if !g.tags.iter().any(|t| &t.id == tid) {
                        return false;
                    }
                }
                if self.fixes_my_games_only && !my_ids.contains(&g.app_id) {
                    return false;
                }
                q.is_empty() || g.name.to_ascii_lowercase().contains(&q) || g.app_id.contains(&q)
            })
            .cloned()
            .collect();

        let avail_h = (ui.available_height() - 32.0).max(260.0);
        let mut clicked_fix_game: Option<u32> = None;

        ui.columns(2, |cols| {
            Frame::new()
                .fill(Palette::BG_PANEL)
                .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                .inner_margin(Margin::same(8))
                .show(&mut cols[0], |ui| {
                    ui.set_height(avail_h);
                    ScrollArea::vertical()
                        .id_salt("fixes_games_scroll")
                        .show(ui, |ui| {
                            for g in &filtered_games {
                                let aid_num = g.app_id.parse::<u32>().ok();
                                let sel = aid_num.is_some() && self.fixes_current_app == aid_num;
                                if ui
                                    .selectable_label(
                                        sel,
                                        format!(
                                            "{}   [{}]  ·  {} fix",
                                            g.name, g.app_id, g.fix_count
                                        ),
                                    )
                                    .clicked()
                                {
                                    clicked_fix_game = aid_num;
                                }
                            }
                        });
                });

            Frame::new()
                .fill(Palette::BG_PANEL)
                .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
                .inner_margin(Margin::same(12))
                .show(&mut cols[1], |ui| {
                    ui.set_height(avail_h);
                    if let Some(aid) = self.fixes_current_app {
                        let install_dir = self.steam_service.find_install_dir(aid);
                        ui.label(
                            RichText::new(format!(
                                "{}  [AppID {}]",
                                self.resolve_game_title(aid),
                                aid
                            ))
                            .strong()
                            .size(16.0),
                        );
                        ui.label(
                            RichText::new(match &install_dir {
                                Some(p) => format!("INSTALL DIR :: {}", p.display()),
                                None => {
                                    "INSTALL DIR :: NOT INSTALLED LOCALLY (MANIFESTS STILL USABLE)"
                                        .into()
                                }
                            })
                            .monospace()
                            .size(10.5)
                            .color(Palette::TEXT_MUTED),
                        );
                        ui.add_space(6.0);

                        let items = self.fixes_items.clone();
                        let mut act_mf = None;
                        let mut act_fix = None;
                        let mut act_rev = None;

                        TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(Layout::left_to_right(Align::Center))
                            .column(Column::remainder().at_least(150.0))
                            .column(Column::exact(95.0))
                            .column(Column::exact(225.0))
                            .header(26.0, |mut h| {
                                for lbl in ["FIX TITLE", "STATUS", "ACTIONS"] {
                                    h.col(|ui| {
                                        ui.label(
                                            RichText::new(lbl)
                                                .monospace()
                                                .strong()
                                                .size(10.5)
                                                .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                }
                            })
                            .body(|mut body| {
                                for (idx, fx) in items.iter().enumerate() {
                                    let applied = install_dir
                                        .as_ref()
                                        .map(|d| FixesService::is_fix_applied(d, &fx.id))
                                        .unwrap_or(false);
                                    body.row(36.0, |mut row| {
                                        row.col(|ui| {
                                            if ui
                                                .selectable_label(
                                                    self.fixes_selected_row == Some(idx),
                                                    &fx.title,
                                                )
                                                .clicked()
                                            {
                                                self.fixes_selected_row = Some(idx);
                                            }
                                        });
                                        row.col(|ui| {
                                            let (st, col) = if applied {
                                                ("APPLIED", Palette::BLUE_LIGHT)
                                            } else if install_dir.is_some() {
                                                ("READY", Palette::TEXT_PRIMARY)
                                            } else {
                                                ("MF ONLY", Palette::TEXT_MUTED)
                                            };
                                            ui.label(
                                                RichText::new(st)
                                                    .monospace()
                                                    .strong()
                                                    .size(11.0)
                                                    .color(col),
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            if fx.has_manifest
                                                && table_button(ui, "MANIFEST", false).clicked()
                                            {
                                                act_mf = Some(fx.clone());
                                            }
                                            if fx.has_fix {
                                                ui.add_enabled_ui(install_dir.is_some(), |ui| {
                                                    if table_button(ui, "APPLY FIX", true).clicked()
                                                    {
                                                        act_fix = Some((
                                                            fx.clone(),
                                                            install_dir.clone().unwrap(),
                                                        ));
                                                    }
                                                });
                                            }
                                            if applied {
                                                if let Some(dir) = &install_dir {
                                                    if danger_button(ui, "REVERT").clicked() {
                                                        act_rev =
                                                            Some((fx.id.clone(), dir.clone()));
                                                    }
                                                }
                                            }
                                        });
                                    });
                                }
                            });

                        if let Some(sel) = self.fixes_selected_row {
                            if let Some(fx) = items.get(sel) {
                                ui.add_space(6.0);
                                Frame::new()
                                    .fill(Palette::BG_VOID)
                                    .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                                    .inner_margin(Margin::same(8))
                                    .show(ui, |ui| {
                                        let desc = if fx.description.trim().is_empty() {
                                            "No additional notes."
                                        } else {
                                            fx.description.trim()
                                        };
                                        ui.label(
                                            RichText::new(format!("{} — {}", fx.title, desc))
                                                .size(12.0)
                                                .color(Palette::TEXT_SECONDARY),
                                        );
                                    });
                            }
                        }

                        if let Some(fx) = act_mf {
                            self.apply_fix_manifest_async(aid, fx, ui.ctx());
                        }
                        if let Some((fx, dir)) = act_fix {
                            self.apply_fix_files_async(aid, fx, dir, ui.ctx());
                        }
                        if let Some((fid, dir)) = act_rev {
                            let res = FixesService::unfix_game(&dir, &fid);
                            self.status_text = if res.ok {
                                format!(
                                    "Reverted {} file(s) for AppID {}",
                                    res.applied_files.len(),
                                    aid
                                )
                            } else {
                                res.error.unwrap_or_else(|| "Revert failed".into())
                            };
                        }
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new("SELECT A GAME FROM THE FIX CATALOG")
                                    .monospace()
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    }
                });
        });

        if let Some(aid) = clicked_fix_game {
            self.open_app_in_fixes(aid, ui.ctx());
        }

        if self.fixes_busy {
            ui.add(
                ProgressBar::new(self.fixes_progress)
                    .show_percentage()
                    .corner_radius(egui::CornerRadius::ZERO),
            );
        }
    }

    // ===== 06 // TOOLCHAIN =====

    pub fn render_toolchain(&mut self, ui: &mut egui::Ui) {
        section_header(
            ui,
            "06",
            "EXTERNAL TOOLCHAIN",
            "Auto-managed binaries for CDN depot downloading, emulator configuration, and SteamStub unpacking",
            |_ui| {},
        );
        ui.add_space(8.0);

        for spec in TOOLS {
            let info = self.tools_manager.inspect_tool(spec.key);
            let is_busy = self.tool_busy.get(spec.key).copied().unwrap_or(false);
            let pct = self.tool_progress.get(spec.key).copied().unwrap_or(0.0);

            let resp = panel_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(spec.name.to_ascii_uppercase())
                            .strong()
                            .size(15.0),
                    );
                    ui.label(
                        RichText::new(format!("github.com/{}", spec.github_repo))
                            .monospace()
                            .size(11.0)
                            .color(Palette::TEXT_MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if info.installed {
                            status_badge(
                                ui,
                                &format!("INSTALLED · {}", info.version),
                                BadgeTone::Blue,
                            );
                        } else {
                            status_badge(ui, "NOT INSTALLED", BadgeTone::Muted);
                        }
                    });
                });

                ui.label(
                    RichText::new(spec.description)
                        .size(12.0)
                        .color(Palette::TEXT_SECONDARY),
                );
                let path_txt = match &info.exe_path {
                    Some(p) => format!("PATH :: {}  [{}]", p.display(), info.runtime_note),
                    None => format!(
                        "PATH :: {}",
                        self.tools_manager.tools_dir.join(spec.key).display()
                    ),
                };
                ui.label(
                    RichText::new(path_txt)
                        .monospace()
                        .size(10.5)
                        .color(Palette::TEXT_MUTED),
                );

                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!is_busy, |ui| {
                        if primary_button(ui, "INSTALL / UPDATE").clicked() {
                            self.update_external_tool_async(spec.key, ui.ctx());
                        }
                    });
                    if ghost_button(ui, "OPEN TOOL DIR").clicked() {
                        let d = self.tools_manager.tools_dir.join(spec.key);
                        let _ = std::fs::create_dir_all(&d);
                        let _ = Command::new("xdg-open").arg(&d).spawn();
                    }
                    if spec.key == "steamautocrack"
                        && secondary_button(ui, "LAUNCH ON GAME FOLDER...").clicked()
                    {
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        thread::spawn(move || {
                            if let Some(dir) =
                                pick_directory_native("Select Game Directory for SteamAutoCrack")
                            {
                                let _ = tx.send(AppMessage::FolderPickedForAutoCrack(dir));
                                ctx.request_repaint();
                            }
                        });
                    }
                    if spec.key == "steamless"
                        && secondary_button(ui, "UNPACK EXECUTABLE (.EXE)...").clicked()
                    {
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        thread::spawn(move || {
                            let files =
                                pick_files_native("Select Executable (.exe)", Some("*.exe"));
                            if let Some(f) = files.into_iter().next() {
                                let _ = tx.send(AppMessage::FilePickedForSteamless(f));
                                ctx.request_repaint();
                            }
                        });
                    }
                });

                if is_busy {
                    ui.add(
                        ProgressBar::new(pct)
                            .show_percentage()
                            .corner_radius(egui::CornerRadius::ZERO),
                    );
                }
            });

            if info.installed {
                let r = resp.response.rect;
                ui.painter().line_segment(
                    [r.left_top(), r.right_top()],
                    Stroke::new(2.0_f32, Palette::BLUE_HOVER),
                );
            }
            ui.add_space(6.0);
        }
    }

    // ===== 07 // SYSTEM SETTINGS =====

    pub fn render_settings(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical()
            .id_salt("settings_scroll")
            .show(ui, |ui| {
                section_header(
                    ui,
                    "07",
                    "SYSTEM & INTEGRATION CONFIGURATION",
                    "Authentication, Hubcap API, Steam installation override, SLSsteam unlocker, and Millennium / CEF bridges",
                    |_ui| {},
                );
                ui.add_space(8.0);

                // 1. Account
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("LUATOOLS ACCOUNT (DISCORD OAUTH & BOT CODE)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        if self.auth_service.is_signed_in() {
                            status_badge(ui, "SIGNED IN", BadgeTone::Blue);
                            ui.label(
                                RichText::new(format!(
                                    "{} ({})",
                                    self.auth_service.display_name(),
                                    self.auth_service.email()
                                ))
                                .strong(),
                            );
                        } else {
                            status_badge(ui, "GUEST", BadgeTone::Neutral);
                            ui.label("Guest mode (sign in for lua.tools API downloads)");
                        }
                        if !self.settings_usage_text.is_empty() {
                            ui.label(
                                RichText::new(&self.settings_usage_text)
                                    .monospace()
                                    .size(11.0)
                                    .color(Palette::BLUE_LIGHT),
                            );
                        }
                    });
                    ui.horizontal(|ui| {
                        if primary_button(ui, "SIGN IN WITH DISCORD (BROWSER)").clicked() {
                            let auth = self.auth_service.clone();
                            let tx = self.msg_tx.clone();
                            let ctx = ui.ctx().clone();
                            self.status_text = "Waiting for Discord OAuth in browser...".into();
                            thread::spawn(move || {
                                match auth.sign_in_with_browser(180) {
                                    Ok(s) => {
                                        let _ = tx.send(AppMessage::AuthCompleted {
                                            ok: true,
                                            msg: format!("Signed in as {}", s.display_name),
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMessage::AuthCompleted {
                                            ok: false,
                                            msg: e,
                                        });
                                    }
                                }
                                ctx.request_repaint();
                            });
                        }
                        if secondary_button(ui, "SIGN IN WITH 6-CHAR BOT CODE").clicked() {
                            self.text_prompt_modal.open = true;
                            self.text_prompt_modal.kind = "bot_code".into();
                            self.text_prompt_modal.title = "DISCORD BOT CODE".into();
                            self.text_prompt_modal.prompt =
                                "Enter the 6-character code from Discord /login:".into();
                            self.text_prompt_modal.value.clear();
                        }
                        if self.auth_service.is_signed_in()
                            && danger_button(ui, "SIGN OUT").clicked()
                        {
                            self.auth_service.sign_out();
                            self.settings_usage_text.clear();
                            self.status_text = "Signed out.".into();
                        }
                    });
                });

                ui.add_space(8.0);

                // 2. Hubcap
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("HUBCAP MANIFEST PROVIDER (HUBCAPMANIFEST.COM)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        ui.label("API Key:");
                        ui.add(
                            TextEdit::singleline(&mut self.settings_service.settings.hubcap_api_key)
                                .password(!self.settings_show_hubcap)
                                .hint_text("smm_... (96 hex characters)")
                                .desired_width(ui.available_width() - 235.0),
                        );
                        if ghost_button(
                            ui,
                            if self.settings_show_hubcap {
                                "HIDE"
                            } else {
                                "SHOW"
                            },
                        )
                        .clicked()
                        {
                            self.settings_show_hubcap = !self.settings_show_hubcap;
                        }
                        if secondary_button(ui, "VERIFY KEY & QUOTA").clicked() {
                            let key = self.settings_service.settings.hubcap_api_key.trim().to_string();
                            if !HubcapService::is_valid_key_format(&key) {
                                self.settings_hubcap_status =
                                    "Invalid format — expected smm_ + 96 lowercase hex chars".into();
                            } else {
                                let hc = self.hubcap_service.clone();
                                let tx = self.msg_tx.clone();
                                let ctx = ui.ctx().clone();
                                self.settings_hubcap_status = "Checking Hubcap stats...".into();
                                thread::spawn(move || {
                                    let msg = if let Some(st) = hc.get_stats(&key) {
                                        (
                                            true,
                                            format!(
                                                "VALID KEY · USAGE: {}/{} · ACTIVE: {}",
                                                st.daily_usage, st.daily_limit, st.can_make_requests
                                            ),
                                        )
                                    } else {
                                        (false, "Could not verify Hubcap key.".into())
                                    };
                                    let _ = tx.send(AppMessage::HubcapChecked {
                                        ok: msg.0,
                                        msg: msg.1,
                                    });
                                    ctx.request_repaint();
                                });
                            }
                        }
                    });
                    if !self.settings_hubcap_status.is_empty() {
                        ui.label(
                            RichText::new(&self.settings_hubcap_status)
                                .monospace()
                                .size(11.0)
                                .color(Palette::BLUE_LIGHT),
                        );
                    }
                });

                ui.add_space(8.0);

                // 3. Steam Override & Behavior
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("STEAM INSTALLATION & LUATOOLS BEHAVIOR")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Steam Path Override:");
                        ui.add(
                            TextEdit::singleline(
                                &mut self.settings_service.settings.steam_path_override,
                            )
                            .hint_text(
                                "Leave empty for auto-detect (~/.local/share/Steam, ~/.steam/steam, Flatpak)",
                            )
                            .desired_width(ui.available_width() - 105.0),
                        );
                        if secondary_button(ui, "BROWSE...").clicked() {
                            let tx = self.msg_tx.clone();
                            let ctx = ui.ctx().clone();
                            thread::spawn(move || {
                                if let Some(dir) = pick_directory_native("Select Steam Root Folder")
                                {
                                    let _ = tx.send(AppMessage::FolderPickedForSteamOverride(dir));
                                    ctx.request_repaint();
                                }
                            });
                        }
                    });
                    ui.checkbox(
                        &mut self.settings_service.settings.auto_update_apps,
                        "Auto-Update Apps (comment out setManifestid pins when installing standard .lua files)",
                    );
                    ui.checkbox(
                        &mut self.settings_service.settings.fast_fetch,
                        "Enable FastFetch (automatically select first available manifest source)",
                    );
                    ui.checkbox(
                        &mut self.settings_service.settings.donate_keys,
                        "Contribute newly discovered depot decryption keys from Steam config.vdf",
                    );
                });

                ui.add_space(8.0);

                // 4. SLSsteam Config
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("SLSSTEAM UNLOCKER (~/.CONFIG/SLSSTEAM/CONFIG.YAML)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    let cfg = &mut self.slssteam_service.config;
                    ui.label(
                        RichText::new(format!(
                            "TRACKED :: {} AdditionalApps · {} FakeAppIds · {} DepotKeys · {} AppTokens",
                            cfg.additional_apps.len(),
                            cfg.fake_app_ids.len(),
                            cfg.depot_keys.len(),
                            cfg.app_tokens.len()
                        ))
                        .monospace()
                        .size(11.0)
                        .color(Palette::TEXT_MUTED),
                    );
                    ui.checkbox(
                        &mut cfg.play_not_owned,
                        "Allow playing unowned games (PlayNotOwnedGames: true)",
                    );
                    ui.checkbox(
                        &mut cfg.limit_downloads,
                        "Restrict downloads to Steam Library folders (LimitDownloadsToSteamLibrary)",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Workshop / Downloader Tool:");
                        ui.add(
                            TextEdit::singleline(&mut cfg.workshop_path)
                                .hint_text("/path/to/DepotDownloaderMod")
                                .desired_width(ui.available_width() - 105.0),
                        );
                        if secondary_button(ui, "BROWSE...").clicked() {
                            let tx = self.msg_tx.clone();
                            let ctx = ui.ctx().clone();
                            thread::spawn(move || {
                                let files = pick_files_native("Select DepotDownloaderMod", None);
                                if let Some(f) = files.into_iter().next() {
                                    let _ = tx.send(AppMessage::FilePickedForWorkshop(f));
                                    ctx.request_repaint();
                                }
                            });
                        }
                    });
                });

                ui.add_space(8.0);

                // 5. Millennium / CEF / Protocol
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("STEAM STORE INTEGRATION (MILLENNIUM RPC, CEF CDP & PROTOCOL)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.label(
                        RichText::new(&self.settings_millennium_status)
                            .monospace()
                            .size(11.0)
                            .color(if self.millennium_connected {
                                Palette::BLUE_LIGHT
                            } else {
                                Palette::TEXT_MUTED
                            }),
                    );
                    ui.checkbox(
                        &mut self.settings_service.settings.cef_injector_enabled,
                        "Enable CEF Chrome DevTools Store Injector (polls 127.0.0.1:8080 -> local RPC :6767)",
                    );
                    ui.horizontal(|ui| {
                        if secondary_button(ui, "PROBE MILLENNIUM").clicked() {
                            self.probe_bridge_async(ui.ctx());
                        }
                        if secondary_button(ui, "ENABLE STEAM CEF DEBUGGING").clicked() {
                            if let Some(inst) = &self.steam_service.installation {
                                match CefInjectorService::ensure_cef_debugging_enabled(&inst.root) {
                                    Ok(m) => self.status_text = m,
                                    Err(e) => self.status_text = e,
                                }
                            } else {
                                self.status_text = "Steam installation not detected.".into();
                            }
                        }
                        if secondary_button(ui, "REGISTER luatools:// HANDLER").clicked() {
                            match ProtocolService::register_desktop_handler() {
                                Ok(m) => self.status_text = m,
                                Err(e) => self.status_text = e,
                            }
                        }
                        if ghost_button(ui, "RESTART STEAM").clicked() {
                            self.trigger_restart_steam(ui.ctx());
                        }
                    });
                });

                ui.add_space(8.0);

                // 6. Git Self-Updater
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("APPLICATION SELF-UPDATER (GIT PULL & NATIVE REBUILD)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.label(
                        RichText::new(&self.settings_update_status)
                            .monospace()
                            .size(11.0)
                            .color(Palette::BLUE_LIGHT),
                    );
                    ui.checkbox(
                        &mut self.settings_service.settings.check_updates_on_startup,
                        "Check Git remote once in the background on startup (shows compact ⬆ UPDATE button in bottom-right corner)",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Git Remote URL:");
                        ui.add(
                            TextEdit::singleline(
                                &mut self.settings_service.settings.git_remote_url,
                            )
                            .hint_text(
                                "Optional if local repo already has 'origin' (e.g. https://github.com/user/luatools-linux.git)",
                            )
                            .desired_width(ui.available_width() - 10.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Local Repo Dir:");
                        ui.add(
                            TextEdit::singleline(
                                &mut self.settings_service.settings.update_repo_dir,
                            )
                            .hint_text(
                                "Auto-detected from install.sh or ~/.local/share/luatools/repo",
                            )
                            .desired_width(ui.available_width() - 105.0),
                        );
                        if secondary_button(ui, "BROWSE...").clicked() {
                            let tx = self.msg_tx.clone();
                            let ctx = ui.ctx().clone();
                            thread::spawn(move || {
                                if let Some(dir) =
                                    pick_directory_native("Select Local Git Repository Folder")
                                {
                                    let _ = tx.send(AppMessage::FolderPickedForUpdateRepo(dir));
                                    ctx.request_repaint();
                                }
                            });
                        }
                    });
                    ui.horizontal(|ui| {
                        if secondary_button(ui, "CHECK FOR UPDATES").clicked() {
                            let _ = self.settings_service.save();
                            self.check_app_update_once_async(ui.ctx());
                        }
                        if primary_button(ui, "GIT PULL & UPDATE NOW").clicked() {
                            let _ = self.settings_service.save();
                            self.trigger_git_self_update(ui.ctx());
                        }
                    });
                });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "SAVE ALL CONFIGURATION").clicked() {
                        let sls_ok = self.slssteam_service.save();
                        let app_ok = self.settings_service.save();
                        if self.settings_service.settings.cef_injector_enabled {
                            self.cef_injector.start();
                        } else {
                            self.cef_injector.stop();
                        }
                        self.detect_steam_and_sync();
                        self.status_text = if sls_ok && app_ok {
                            "SAVED :: Written ~/.config/luatools/settings.json and ~/.config/SLSsteam/config.yaml".into()
                        } else {
                            "ERROR :: Failed to write one or more configuration files".into()
                        };
                    }
                });
            });
    }
}
