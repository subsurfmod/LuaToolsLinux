//! Workbench Workspaces: `Depots & Vault`, `Game Fixes`, and Tabbed `Settings` (including Toolchain & Git Self-Updater).

use std::path::PathBuf;
use std::process::Command;
use std::thread;

use eframe::egui::{
    self, Align, Checkbox, ComboBox, CornerRadius, FontFamily, FontId, Layout, Margin, ProgressBar,
    RichText, ScrollArea, TextEdit,
};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use crate::parsers::lua_parser::LuaFileParser;
use crate::services::api_client::HubcapService;
use crate::services::cef_bridge::CefBridgeService;
use crate::services::cloud_redirect_service::CloudRedirectStatus;
use crate::services::depot_downloader::{DepotDownloadOptions, DepotSelection};
use crate::services::external_tools::TOOLS;
use crate::services::fixes_service::FixesService;
use crate::services::protocol_service::ProtocolService;
use crate::services::update_service::UpdateService;
use crate::ui::app::LuaToolsApp;
use crate::ui::state::{AppMessage, DepotsSubTab, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    chip_button, danger_button, display_font, draw_rem_medallion, format_bytes, ghost_button,
    inset_paper_sheet, lua_layouter, paper_card, paper_card_padded, pick_directory_native,
    pick_files_native, primary_button, secondary_button, section_header, semibold_font,
    status_badge, table_button, BadgeTone, Palette,
};
use crate::ui::views_primary::clean_list_row;

impl LuaToolsApp {
    // ===== 4. DEPOTS & LUAVAULT WORKBENCH =====

    pub fn render_depots(&mut self, ui: &mut egui::Ui) {
        let mut do_refresh = false;
        section_header(
            ui,
            "WORKBENCH",
            "Depots & LuaVault",
            "Switch stored manifest builds, edit live stplug-in .lua files, and download Steam depots",
            |ui| {
                if self.depots_app_id.is_some() && ghost_button(ui, "Reload").clicked() {
                    do_refresh = true;
                }
            },
        );
        if do_refresh {
            if let Some(aid) = self.depots_app_id {
                self.open_game_in_depots(aid, ui.ctx());
            }
        }

        // Clean Active Title & Mode Bar (non-overlapping Sides)
        let mut switch_to_app: Option<u32> = None;
        let mut jump_to_app: Option<u32> = None;
        let mut new_subtab: Option<DepotsSubTab> = None;
        let cur_label = match self.depots_app_id {
            Some(aid) => format!("{}  (#{})", self.resolve_game_title(aid), aid),
            None => "Select a configured game…".into(),
        };
        let mut known_games: Vec<(u32, String)> = Vec::new();
        {
            let mut ids: Vec<u32> = self.recent_installed.iter().map(|(id, _)| *id).collect();
            for r in &self.library_rows {
                if r.has_lua && !ids.contains(&r.app_id) {
                    ids.push(r.app_id);
                }
            }
            for id in ids {
                known_games.push((id, format!("{}  (#{})", self.resolve_game_title(id), id)));
            }
        }
        let active_aid = self.depots_app_id;
        let cur_subtab = self.depots_subtab;

        paper_card_padded(ui, Margin::symmetric(16, 12), |ui| {
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new("Game:")
                            .font(semibold_font(12.5))
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ComboBox::from_id_salt("depots_active_game_combo")
                        .width(260.0)
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            for (id, txt) in &known_games {
                                if ui.selectable_label(active_aid == Some(*id), txt).clicked() {
                                    switch_to_app = Some(*id);
                                }
                            }
                        });

                    let jump = ui.add_sized(
                        [115.0, 28.0],
                        TextEdit::singleline(&mut self.depots_jump_input)
                            .margin(Margin::symmetric(8, 5))
                            .hint_text("Jump AppID…"),
                    );
                    if (jump.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        || ghost_button(ui, "Load").clicked()
                    {
                        if let Ok(aid) = self.depots_jump_input.trim().parse::<u32>() {
                            jump_to_app = Some(aid);
                        }
                    }
                },
                |ui| {
                    // Sides right closure runs right-to-left, so place rightmost tab first
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if chip_button(
                        ui,
                        "Manifest Binding",
                        cur_subtab == DepotsSubTab::ManifestBinding,
                    )
                    .clicked()
                    {
                        new_subtab = Some(DepotsSubTab::ManifestBinding);
                    }
                    if chip_button(
                        ui,
                        "Download Depots",
                        cur_subtab == DepotsSubTab::DownloadPicker,
                    )
                    .clicked()
                    {
                        new_subtab = Some(DepotsSubTab::DownloadPicker);
                    }
                    if chip_button(ui, "Lua Editor", cur_subtab == DepotsSubTab::LiveEditor)
                        .clicked()
                    {
                        new_subtab = Some(DepotsSubTab::LiveEditor);
                    }
                    if chip_button(
                        ui,
                        "Manifests & Vault",
                        cur_subtab == DepotsSubTab::Declarations,
                    )
                    .clicked()
                    {
                        new_subtab = Some(DepotsSubTab::Declarations);
                    }
                },
            );
        });

        if let Some(st) = new_subtab {
            self.depots_subtab = st;
            if st == DepotsSubTab::DownloadPicker {
                self.rebuild_depot_picks();
            }
        }

        if let Some(aid) = switch_to_app.or(jump_to_app) {
            self.depots_jump_input.clear();
            self.open_game_in_depots(aid, ui.ctx());
        }

        ui.add_space(16.0);

        let Some(app_id) = self.depots_app_id else {
            paper_card_padded(ui, Margin::symmetric(24, 48), |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    draw_rem_medallion(ui, 48.0);
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("No active game selected")
                            .font(display_font(18.0))
                            .color(Palette::TEXT_PRIMARY),
                    );
                    ui.label(
                        RichText::new(
                            "Pick a configured game from the selector above or enter an AppID.",
                        )
                        .size(13.0)
                        .color(Palette::TEXT_MUTED),
                    );
                });
            });
            return;
        };

        match self.depots_subtab {
            DepotsSubTab::Declarations => self.render_depots_declarations_tab(ui, app_id),
            DepotsSubTab::LiveEditor => self.render_depots_editor_tab(ui, app_id),
            DepotsSubTab::DownloadPicker => self.render_depots_downloader_tab(ui, app_id),
            DepotsSubTab::ManifestBinding => self.render_depots_manifest_binding_tab(ui, app_id),
        }
    }

    fn render_depots_manifest_binding_tab(&mut self, ui: &mut egui::Ui, app_id: u32) {
        let pins = self.manifest_binding_service.get_pins_for_app(app_id);
        let active_n = pins.iter().filter(|p| p.active_in_bridge).count();
        let mut do_resync = false;
        let mut do_add_pin = false;
        let mut do_remove_depot: Option<u32> = None;

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Depot Manifest Binding & Enforcement")
                                .font(display_font(17.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} pinned depot(s) for AppID {}  ·  {} active in Steam bridge, {} pending Steam launch",
                                pins.len(),
                                app_id,
                                active_n,
                                pins.len().saturating_sub(active_n)
                            ))
                            .size(12.5)
                            .color(Palette::TEXT_MUTED),
                        );
                    });
                },
                |ui| {
                    if secondary_button(ui, "Re-Sync Pins to Bridge").clicked() {
                        do_resync = true;
                    }
                },
            );

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label("Depot ID:");
                ui.add_sized(
                    [110.0, 30.0],
                    TextEdit::singleline(&mut self.binding_depot_input)
                        .margin(Margin::symmetric(8, 6))
                        .hint_text(format!("{}", app_id + 1)),
                );
                ui.label("Manifest GID:");
                ui.add_sized(
                    [230.0, 30.0],
                    TextEdit::singleline(&mut self.binding_gid_input)
                        .margin(Margin::symmetric(8, 6))
                        .hint_text("19-digit Steam manifest GID"),
                );
                ui.label("Size:");
                ui.add_sized(
                    [125.0, 30.0],
                    TextEdit::singleline(&mut self.binding_size_input)
                        .margin(Margin::symmetric(8, 6))
                        .hint_text("Bytes (optional)"),
                );
                if primary_button(ui, "+ Add Manifest Pin").clicked() {
                    do_add_pin = true;
                }
            });
        });

        if do_resync {
            let bound = self.manifest_binding_service.resync_all_to_bridge();
            self.notify(
                format!(
                    "Synced stored manifest pins to Steam IPC ({} active in bridge).",
                    bound
                ),
                ToastKind::Info,
            );
        }
        if do_add_pin {
            let depot_opt = self.binding_depot_input.trim().parse::<u32>().ok();
            let gid = self.binding_gid_input.trim().to_string();
            let sz = self.binding_size_input.trim().parse::<u64>().unwrap_or(0);
            if let Some(did) = depot_opt {
                if !gid.is_empty() {
                    let active =
                        self.manifest_binding_service
                            .set_pin(app_id, did, gid.clone(), sz);
                    self.binding_depot_input.clear();
                    self.binding_gid_input.clear();
                    self.binding_size_input.clear();
                    self.notify(
                        format!(
                            "Pinned Depot {} → {} ({})",
                            did,
                            gid,
                            if active {
                                "Active in Steam"
                            } else {
                                "Pending / Saved"
                            }
                        ),
                        ToastKind::Success,
                    );
                } else {
                    self.notify("Enter a Manifest GID to pin.", ToastKind::Warning);
                }
            } else {
                self.notify("Enter a valid numeric Depot ID.", ToastKind::Warning);
            }
        }

        ui.add_space(14.0);

        paper_card_padded(ui, Margin::symmetric(14, 12), |ui| {
            ui.set_width(ui.available_width());
            let table_h = (ui.available_height() - 16.0).max(200.0);
            ui.set_min_height(table_h);

            let cur_pins = self.manifest_binding_service.get_pins_for_app(app_id);
            if cur_pins.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(44.0);
                    ui.label(
                        RichText::new("No manifest pins registered for this game")
                            .font(semibold_font(14.0))
                            .color(Palette::TEXT_PRIMARY),
                    );
                    ui.label(
                        RichText::new(
                            "Add a pin above or enable setManifestid(...) in Manifests & Vault.",
                        )
                        .size(12.5)
                        .color(Palette::TEXT_MUTED),
                    );
                });
            } else {
                TableBuilder::new(ui)
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(115.0))
                    .column(Column::remainder().at_least(210.0))
                    .column(Column::exact(120.0))
                    .column(Column::exact(165.0))
                    .column(Column::exact(110.0))
                    .header(30.0, |mut h| {
                        for col in ["Depot ID", "Pinned Manifest GID", "Size", "Bind Status", ""] {
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
                        for pin in &cur_pins {
                            body.row(38.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(pin.depot_id.to_string())
                                            .font(FontId::new(12.5, FontFamily::Monospace))
                                            .color(Palette::TEXT_PRIMARY),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(&pin.manifest_id)
                                            .font(FontId::new(12.5, FontFamily::Monospace))
                                            .color(Palette::BLUE_LIGHT),
                                    );
                                });
                                row.col(|ui| {
                                    let sz_lbl = if pin.size > 0 {
                                        format_bytes(pin.size)
                                    } else {
                                        "—".into()
                                    };
                                    ui.label(
                                        RichText::new(sz_lbl)
                                            .size(12.0)
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                                row.col(|ui| {
                                    if pin.active_in_bridge {
                                        status_badge(ui, "Active (Bound)", BadgeTone::Ready);
                                    } else {
                                        status_badge(ui, "Pending (Saved)", BadgeTone::Neutral);
                                    }
                                });
                                row.col(|ui| {
                                    if table_button(ui, "Remove Pin", false).clicked() {
                                        do_remove_depot = Some(pin.depot_id);
                                    }
                                });
                            });
                        }
                    });
            }
        });

        if let Some(did) = do_remove_depot {
            self.manifest_binding_service.remove_pin(app_id, did);
            self.notify(
                format!("Removed manifest pin for Depot {}.", did),
                ToastKind::Info,
            );
        }
    }

    fn render_depots_declarations_tab(&mut self, ui: &mut egui::Ui, app_id: u32) {
        let body_h = (ui.available_height() - 6.0).max(260.0);

        StripBuilder::new(ui)
            .size(Size::exact(275.0))
            .size(Size::exact(16.0))
            .size(Size::remainder().at_least(340.0))
            .horizontal(|mut strip| {
                // Left: LuaVault Variants Card
                strip.cell(|ui| {
                    paper_card_padded(ui, Margin::same(14), |ui| {
                        ui.set_height(body_h - 28.0);
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(
                                    RichText::new("LuaVault")
                                        .font(display_font(16.5))
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            },
                            |ui| {
                                if table_button(ui, "+ Save Preset", false).clicked() {
                                    self.text_prompt_modal.open = true;
                                    self.text_prompt_modal.kind = "preset".into();
                                    self.text_prompt_modal.title = "Save LuaVault Preset".into();
                                    self.text_prompt_modal.prompt =
                                        "Name this .lua build/preset:".into();
                                    self.text_prompt_modal.value.clear();
                                }
                            },
                        );
                        ui.label(
                            RichText::new("Stored .lua builds & snapshots")
                                .size(12.0)
                                .color(Palette::TEXT_MUTED),
                        );
                        ui.add_space(10.0);

                        let active_hash = self.lua_vault.get_active_hash(app_id);
                        let variants = self.depots_variants.clone();
                        let mut apply_hash: Option<String> = None;
                        let mut rename_hash: Option<(String, String)> = None;
                        let mut delete_hash: Option<String> = None;

                        ScrollArea::vertical()
                            .id_salt("vault_variants_scroll")
                            .max_height((body_h - 140.0).max(120.0))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                if variants.is_empty() {
                                    ui.label(
                                        RichText::new("No stored variants yet.")
                                            .size(12.5)
                                            .color(Palette::TEXT_MUTED),
                                    );
                                }
                                for v in &variants {
                                    let is_live = active_hash.as_deref() == Some(v.hash.as_str());
                                    let is_sel = self.depots_selected_variant_hash.as_deref()
                                        == Some(v.hash.as_str());
                                    let meta = if is_live {
                                        "Live".to_string()
                                    } else if v.is_pinned() {
                                        format!("{} pinned", v.manifest_ids.len())
                                    } else {
                                        format!("{} depots", v.depot_count)
                                    };
                                    let resp = clean_list_row(
                                        ui,
                                        &v.display_label(),
                                        &meta,
                                        is_sel || is_live,
                                    );
                                    if resp.clicked() {
                                        self.depots_selected_variant_hash = Some(v.hash.clone());
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button("Apply Variant").clicked() {
                                            apply_hash = Some(v.hash.clone());
                                            ui.close_menu();
                                        }
                                        if ui.button("Rename…").clicked() {
                                            rename_hash = Some((
                                                v.hash.clone(),
                                                v.label.clone().unwrap_or_default(),
                                            ));
                                            ui.close_menu();
                                        }
                                        if !is_live
                                            && ui
                                                .button(
                                                    RichText::new("Delete")
                                                        .color(Palette::DANGER_TEXT),
                                                )
                                                .clicked()
                                        {
                                            delete_hash = Some(v.hash.clone());
                                            ui.close_menu();
                                        }
                                    });
                                }
                            });

                        ui.add_space(10.0);
                        if let Some(ref sel_h) = self.depots_selected_variant_hash.clone() {
                            let is_already_live = active_hash.as_deref() == Some(sel_h.as_str());
                            ui.add_enabled_ui(!is_already_live, |ui| {
                                if primary_button(ui, "Apply Selected Variant").clicked() {
                                    apply_hash = Some(sel_h.clone());
                                }
                            });
                        }

                        if let Some(h) = apply_hash {
                            if self.lua_vault.apply(app_id, &h) {
                                let live_txt = self.lua_vault.read_live_text(app_id);
                                if !live_txt.trim().is_empty() {
                                    let parsed = LuaFileParser::parse_text(&live_txt, app_id);
                                    crate::services::lua_installer::LuaInstaller::sync_with_slssteam(
                                        &mut self.slssteam_service,
                                        app_id,
                                        &parsed,
                                    );
                                }
                                self.reload_depots_local(app_id);
                                self.refresh_all_local_state();
                                self.notify(
                                    format!("Applied LuaVault variant for AppID {}", app_id),
                                    ToastKind::Success,
                                );
                            }
                        }
                        if let Some((h, cur)) = rename_hash {
                            self.text_prompt_modal.open = true;
                            self.text_prompt_modal.kind = "rename_variant".into();
                            self.text_prompt_modal.title = "Rename Variant".into();
                            self.text_prompt_modal.prompt = "New label:".into();
                            self.text_prompt_modal.value = cur;
                            self.text_prompt_modal.target_hash = h;
                        }
                        if let Some(h) = delete_hash {
                            if self.lua_vault.delete(app_id, &h) {
                                self.depots_variants = self.lua_vault.get_variants(app_id);
                                self.depots_selected_variant_hash = None;
                            }
                        }
                    });
                });

                strip.empty();

                // Right: Depot & DLC Declarations Table
                strip.cell(|ui| {
                    paper_card_padded(ui, Margin::same(16), |ui| {
                        ui.set_height(body_h - 32.0);
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(
                                    RichText::new("Depot & DLC Declarations")
                                        .font(display_font(16.5))
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            },
                            |ui| {
                                if ghost_button(ui, "Open Lua Editor ›").clicked() {
                                    self.depots_subtab = DepotsSubTab::LiveEditor;
                                }
                            },
                        );
                        ui.add_space(10.0);

                        let Some(lua) = self.depots_lua.clone() else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(48.0);
                                ui.label(
                                    RichText::new(
                                        "No stplug-in .lua file exists for this title yet.",
                                    )
                                    .color(Palette::TEXT_MUTED),
                                );
                                ui.add_space(8.0);
                                if primary_button(ui, "Create Starter .lua").clicked() {
                                    let tpl = LuaFileParser::default_lua_template(app_id, "");
                                    let _ = self.lua_vault.write_live(app_id, &tpl);
                                    self.reload_depots_local(app_id);
                                    self.refresh_all_local_state();
                                }
                            });
                            return;
                        };

                        let depot_map: std::collections::HashMap<u32, _> = self
                            .depots_info
                            .as_ref()
                            .map(|i| i.depots.iter().map(|d| (d.id, d.clone())).collect())
                            .unwrap_or_default();

                        let mut toggle_pin: Option<(u32, bool)> = None;
                        let mut inspect_entry = None;

                        TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(Layout::left_to_right(Align::Center))
                            .column(Column::exact(95.0))
                            .column(Column::remainder().at_least(150.0))
                            .column(Column::exact(125.0))
                            .column(Column::exact(140.0))
                            .header(30.0, |mut h| {
                                for lbl in ["ID", "Role / Label", "Pin Status", "Actions"] {
                                    h.col(|ui| {
                                        ui.label(
                                            RichText::new(lbl)
                                                .font(semibold_font(12.0))
                                                .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                }
                            })
                            .body(|mut body| {
                                for e in &lua.entries {
                                    body.row(40.0, |mut row| {
                                        row.col(|ui| {
                                            ui.label(
                                                RichText::new(e.id.to_string())
                                                    .font(FontId::new(12.5, FontFamily::Monospace))
                                                    .color(Palette::TEXT_PRIMARY),
                                            );
                                        });
                                        row.col(|ui| {
                                            let role = if e.id == app_id {
                                                "Base Application".to_string()
                                            } else if let Some(c) = &e.comment {
                                                c.clone()
                                            } else if e.has_key() {
                                                "Encrypted Content Depot".into()
                                            } else {
                                                "DLC / App Entitlement".into()
                                            };
                                            ui.label(
                                                RichText::new(role).color(Palette::TEXT_PRIMARY),
                                            );
                                        });
                                        row.col(|ui| {
                                            if e.manifest_id.is_some() {
                                                status_badge(ui, "Pinned", BadgeTone::Blue);
                                            } else if e.commented_manifest_id.is_some() {
                                                status_badge(ui, "Auto-Update", BadgeTone::Ready);
                                            } else if e.has_key() {
                                                status_badge(ui, "Keyed", BadgeTone::Neutral);
                                            } else {
                                                status_badge(ui, "App / DLC", BadgeTone::Muted);
                                            }
                                        });
                                        row.col(|ui| {
                                            ui.spacing_mut().item_spacing.x = 6.0;
                                            if e.manifest_id.is_some() {
                                                if table_button(ui, "Unpin", false).clicked() {
                                                    toggle_pin = Some((e.id, false));
                                                }
                                            } else if e.commented_manifest_id.is_some()
                                                && table_button(ui, "Pin", true).clicked()
                                            {
                                                toggle_pin = Some((e.id, true));
                                            }
                                            if table_button(ui, "Info", false).clicked() {
                                                inspect_entry = Some((
                                                    depot_map.get(&e.id).cloned(),
                                                    e.clone(),
                                                ));
                                            }
                                        });
                                    });
                                }
                            });

                        if let Some((did, enable_pin)) = toggle_pin {
                            let raw = self.lua_vault.read_live_text(app_id);
                            let (updated, changed) =
                                LuaFileParser::toggle_manifest_pin_in_text(&raw, did, None);
                            if changed && self.lua_vault.write_live(app_id, &updated) {
                                self.reload_depots_local(app_id);
                                self.notify(
                                    format!(
                                        "{} manifest pin on depot {}",
                                        if enable_pin {
                                            "Enabled"
                                        } else {
                                            "Commented out"
                                        },
                                        did
                                    ),
                                    ToastKind::Info,
                                );
                            }
                        }
                        if let Some((dep, ent)) = inspect_entry {
                            self.depot_detail_modal.depot = dep;
                            self.depot_detail_modal.entry = Some(ent);
                            self.depot_detail_modal.open = true;
                        }
                    });
                });
            });
    }

    fn render_depots_editor_tab(&mut self, ui: &mut egui::Ui, app_id: u32) {
        paper_card(ui, |ui| {
            let body_h = (ui.available_height() - 12.0).max(260.0);
            ui.set_min_height(body_h);

            let is_editing = self.depots_editing_live;
            let mut act_start_edit = false;
            let mut act_save_live = false;
            let mut act_save_preset = false;
            let mut act_cancel_edit = false;

            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("stplug-in/{}.lua", app_id))
                                .font(display_font(17.0))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        if is_editing {
                            status_badge(ui, "Editing Live", BadgeTone::Blue);
                        } else {
                            status_badge(ui, "Read-Only", BadgeTone::Neutral);
                        }
                    });
                },
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if !is_editing {
                        if primary_button(ui, "Edit Live .lua").clicked() {
                            act_start_edit = true;
                        }
                    } else {
                        if primary_button(ui, "Save to Steam").clicked() {
                            act_save_live = true;
                        }
                        if secondary_button(ui, "Save as Preset…").clicked() {
                            act_save_preset = true;
                        }
                        if ghost_button(ui, "Cancel").clicked() {
                            act_cancel_edit = true;
                        }
                    }
                },
            );

            if act_start_edit {
                let mut live = self.lua_vault.read_live_text(app_id);
                if live.trim().is_empty() {
                    live = LuaFileParser::default_lua_template(app_id, "");
                }
                self.depots_editor_text = live;
                self.depots_editing_live = true;
            }
            if act_save_live {
                if let Some(installer) = self.lua_installer.clone() {
                    let res = installer.install_lua(
                        app_id,
                        &self.depots_editor_text,
                        true,
                        None,
                        Some("manual-edit"),
                        Some(&mut self.settings_service),
                        Some(&mut self.slssteam_service),
                    );
                    if res.ok {
                        self.depots_editing_live = false;
                        self.reload_depots_local(app_id);
                        self.refresh_all_local_state();
                        self.notify(
                            format!("Saved {}.lua and synced SLSsteam.", app_id),
                            ToastKind::Success,
                        );
                    }
                }
            }
            if act_save_preset {
                self.text_prompt_modal.open = true;
                self.text_prompt_modal.kind = "preset".into();
                self.text_prompt_modal.title = "Save Preset".into();
                self.text_prompt_modal.prompt = "Preset label:".into();
                self.text_prompt_modal.value.clear();
            }
            if act_cancel_edit {
                self.depots_editing_live = false;
                self.reload_depots_local(app_id);
            }

            ui.add_space(12.0);
            let mut layouter =
                |ui: &egui::Ui, string: &str, wrap_width: f32| lua_layouter(ui, string, wrap_width);
            let editor_h = (ui.available_height() - 12.0).max(180.0);
            inset_paper_sheet(ui, Margin::same(12), |ui| {
                ScrollArea::vertical()
                    .id_salt("full_lua_editor_scroll")
                    .max_height(editor_h)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut self.depots_editor_text)
                                .code_editor()
                                .interactive(self.depots_editing_live)
                                .desired_rows(18)
                                .desired_width(f32::INFINITY)
                                .frame(false)
                                .layouter(&mut layouter),
                        );
                    });
            });
        });
    }

    fn render_depots_downloader_tab(&mut self, ui: &mut egui::Ui, _app_id: u32) {
        paper_card(ui, |ui| {
            let body_h = (ui.available_height() - 12.0).max(260.0);
            ui.set_min_height(body_h);

            let mut click_qr_login = false;

            // Output folder + Steam Login / Selection controls
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(
                        RichText::new("Output Folder:")
                            .font(semibold_font(13.0))
                            .color(Palette::TEXT_SECONDARY),
                    );
                    let w = (ui.available_width() - 340.0).clamp(150.0, 480.0);
                    ui.add_sized(
                        [w, 30.0],
                        TextEdit::singleline(&mut self.depots_dest_dir)
                            .margin(Margin::symmetric(10, 6)),
                    );
                    if secondary_button(ui, "Browse…").clicked() {
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        thread::spawn(move || {
                            if let Some(d) = pick_directory_native("Select Depot Download Folder") {
                                let _ = tx.send(AppMessage::FolderPickedForDepotDest(d));
                                ctx.request_repaint();
                            }
                        });
                    }
                },
                |ui| {
                    // Sides right closure runs right-to-left
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if ghost_button(ui, "Clear").clicked() {
                        for p in &mut self.depots_picks {
                            p.is_selected = false;
                        }
                    }
                    if ghost_button(ui, "Select All").clicked() {
                        for p in &mut self.depots_picks {
                            if p.blocked_reason.is_none() {
                                p.is_selected = true;
                            }
                        }
                    }
                    ui.add_enabled_ui(!self.depots_downloading, |ui| {
                        if secondary_button(ui, "Sign In (QR)")
                            .on_hover_text(
                                "Open a terminal once to sign in to official DepotDownloader via Steam QR / Guard (-remember-password)",
                            )
                            .clicked()
                        {
                            click_qr_login = true;
                        }
                    });
                },
            );

            if click_qr_login {
                let _ = self.settings_service.save();
                let wp = self.slssteam_service.config.workshop_path.clone();
                let user = self.settings_service.settings.steam_username.clone();
                match self
                    .depot_downloader
                    .launch_interactive_login(Some(wp.as_str()), &user)
                {
                    Ok(msg) => self.notify(msg, ToastKind::Info),
                    Err(err) => self.notify(err, ToastKind::Warning),
                }
            }

            ui.add_space(12.0);

            // Reserve comfortable bottom room for action buttons + progress bar so low text never clips
            let reserve_bottom = if self.depots_downloading { 96.0 } else { 64.0 };
            let table_h = (ui.available_height() - reserve_bottom).max(140.0);
            inset_paper_sheet(ui, Margin::same(8), |ui| {
                ui.set_min_height(table_h);
                if self.depots_picks.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(
                            RichText::new("No depots declared in this title's configuration.")
                                .color(Palette::TEXT_MUTED),
                        );
                    });
                    return;
                }

                TableBuilder::new(ui)
                    .striped(true)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(36.0))
                    .column(Column::exact(95.0))
                    .column(Column::remainder().at_least(160.0))
                    .column(Column::exact(110.0))
                    .column(Column::exact(175.0))
                    .header(30.0, |mut h| {
                        for lbl in ["", "Depot ID", "Content", "Size", "Readiness"] {
                            h.col(|ui| {
                                ui.label(
                                    RichText::new(lbl)
                                        .font(semibold_font(12.0))
                                        .color(Palette::TEXT_MUTED),
                                );
                            });
                        }
                    })
                    .body(|mut body| {
                        for pick in &mut self.depots_picks {
                            body.row(38.0, |mut row| {
                                row.col(|ui| {
                                    ui.add_enabled_ui(pick.blocked_reason.is_none(), |ui| {
                                        ui.add(Checkbox::without_text(&mut pick.is_selected));
                                    });
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(pick.depot.id.to_string())
                                            .font(FontId::new(12.5, FontFamily::Monospace)),
                                    );
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
                                    ui.label(
                                        RichText::new(format_bytes(pick.size))
                                            .font(FontId::new(12.0, FontFamily::Monospace))
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                                row.col(|ui| {
                                    if let Some(b) = &pick.blocked_reason {
                                        status_badge(ui, b, BadgeTone::Danger);
                                    } else if pick.manifest_path.is_some() {
                                        status_badge(ui, "Local Manifest", BadgeTone::Ready);
                                    } else {
                                        status_badge(ui, "Steam CDN", BadgeTone::Blue);
                                    }
                                });
                            });
                        }
                    });
            });

            ui.add_space(12.0);
            let sel_count = self.depots_picks.iter().filter(|p| p.is_selected).count();
            let dl_status_txt = self.depots_dl_status.clone();
            let is_downloading = self.depots_downloading;
            let acct_hint = {
                let u = self.settings_service.settings.steam_username.trim();
                if u.is_empty() {
                    "Session: anonymous / cached QR (set Steam Username in Settings › Account if needed)".to_string()
                } else {
                    format!("Session: {} (-remember-password)", u)
                }
            };
            let mut click_dl = false;

            egui::Sides::new().show(
                ui,
                |ui| {
                    if !dl_status_txt.is_empty() {
                        ui.label(
                            RichText::new(&dl_status_txt)
                                .size(12.0)
                                .color(Palette::TEXT_SECONDARY),
                        );
                    } else {
                        ui.label(
                            RichText::new(acct_hint)
                                .size(12.0)
                                .color(Palette::TEXT_MUTED),
                        );
                    }
                },
                |ui| {
                    ui.add_enabled_ui(sel_count > 0 && !is_downloading, |ui| {
                        if primary_button(ui, &format!("Download Selected ({})", sel_count))
                            .clicked()
                        {
                            click_dl = true;
                        }
                    });
                },
            );

            if click_dl {
                self.start_selected_depot_downloads(ui.ctx());
            }

            if self.depots_downloading {
                ui.add_space(6.0);
                ui.add(
                    ProgressBar::new(self.depots_dl_pct / 100.0)
                        .show_percentage()
                        .corner_radius(CornerRadius::same(3)),
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
            .filter(|p| p.is_selected && p.blocked_reason.is_none())
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
        let dl_opts = DepotDownloadOptions {
            custom_tool_path: self.slssteam_service.config.workshop_path.clone(),
            steam_username: self.settings_service.settings.steam_username.clone(),
        };
        let mut dd = self.depot_downloader.clone();
        dd.steam_installation = self.steam_service.installation.clone();
        dd.slssteam_service = Some(self.slssteam_service.clone());
        let tx = self.msg_tx.clone();
        let ctx_c = ctx.clone();

        self.depots_downloading = true;
        self.depots_dl_pct = 0.0;
        self.depots_dl_status = "Starting official SteamRE/DepotDownloader…".into();

        thread::spawn(move || {
            let mut results = Vec::new();
            for sel in selections {
                let sub = dest_root.join(format!("depot_{}", sel.depot_id));
                let tx_p = tx.clone();
                let ctx_p = ctx_c.clone();
                let label = format!("Depot {}", sel.depot_id);
                let _ = tx.send(AppMessage::DepotDownloadProgress {
                    depot_label: label.clone(),
                    pct: 0.0,
                    msg: "Preparing depot download…".into(),
                });
                ctx_c.request_repaint();
                let r = dd.download_depot(app_id, &sel, &sub, &dl_opts, move |pct, msg| {
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

    // ===== 5. GAME FIXES =====

    pub fn render_fixes(&mut self, ui: &mut egui::Ui) {
        let mut do_refresh = false;
        section_header(
            ui,
            "FIXES",
            "Game & DRM Fix Catalog",
            "Verified manifests and patches with automatic .luatools-fix SHA-256 backup and clean revert",
            |ui| {
                if secondary_button(ui, "Refresh Catalog").clicked() {
                    do_refresh = true;
                }
            },
        );
        if do_refresh {
            self.fetch_fix_listings_async(ui.ctx());
        }

        egui::Sides::new().show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if chip_button(ui, "All", self.fixes_selected_tag.is_none()).clicked() {
                    self.fixes_selected_tag = None;
                }
                let tags = self.fixes_tags.clone();
                for t in tags {
                    let active = self.fixes_selected_tag.as_deref() == Some(t.id.as_str());
                    if chip_button(ui, &t.name, active).clicked() {
                        self.fixes_selected_tag = if active { None } else { Some(t.id) };
                    }
                }
            },
            |ui| {
                // Sides right closure runs right-to-left: search box on far right, checkbox beside it
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.add_sized(
                    [210.0, 30.0],
                    TextEdit::singleline(&mut self.fixes_search)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text("Filter fixes…"),
                );
                ui.checkbox(&mut self.fixes_my_games_only, "Configured only");
            },
        );

        ui.add_space(14.0);

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

        let body_h = (ui.available_height() - 12.0).max(260.0);
        let mut clicked_fix_game: Option<u32> = None;

        StripBuilder::new(ui)
            .size(Size::exact(310.0))
            .size(Size::exact(16.0))
            .size(Size::remainder().at_least(320.0))
            .horizontal(|mut strip| {
                strip.cell(|ui| {
                    paper_card_padded(ui, Margin::same(10), |ui| {
                        ui.set_height(body_h - 24.0);
                        ScrollArea::vertical()
                            .id_salt("fixes_games_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                if filtered_games.is_empty() {
                                    ui.add_space(32.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("No catalog entries loaded")
                                                .color(Palette::TEXT_MUTED),
                                        );
                                    });
                                }
                                for g in &filtered_games {
                                    let aid_num = g.app_id.parse::<u32>().ok();
                                    let sel =
                                        aid_num.is_some() && self.fixes_current_app == aid_num;
                                    let meta = format!("{} fix", g.fix_count);
                                    if clean_list_row(ui, &g.name, &meta, sel).clicked() {
                                        clicked_fix_game = aid_num;
                                    }
                                }
                            });
                    });
                });

                strip.empty();

                strip.cell(|ui| {
                    paper_card(ui, |ui| {
                        ui.set_height(body_h - 36.0);
                        if let Some(aid) = self.fixes_current_app {
                            let install_dir = self.steam_service.find_install_dir(aid);
                            ui.label(
                                RichText::new(self.resolve_game_title(aid))
                                    .font(display_font(20.0))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(match &install_dir {
                                    Some(p) => format!("AppID {}  ·  {}", aid, p.display()),
                                    None => format!(
                                        "AppID {}  ·  Not installed locally (manifests still usable)",
                                        aid
                                    ),
                                })
                                .size(12.0)
                                .color(Palette::TEXT_MUTED),
                            );
                            ui.add_space(14.0);

                            let items = self.fixes_items.clone();
                            let mut act_mf = None;
                            let mut act_fix = None;
                            let mut act_rev = None;

                            ScrollArea::vertical()
                                .id_salt("fix_items_cards_scroll")
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 10.0;
                                    for fx in &items {
                                        let applied = install_dir
                                            .as_ref()
                                            .map(|d| FixesService::is_fix_applied(d, &fx.id))
                                            .unwrap_or(false);

                                        inset_paper_sheet(ui, Margin::same(14), |ui| {
                                            ui.set_width(ui.available_width());
                                            egui::Sides::new().show(
                                                ui,
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(&fx.title)
                                                            .font(semibold_font(14.0))
                                                            .color(Palette::TEXT_PRIMARY),
                                                    );
                                                },
                                                |ui| {
                                                    if applied {
                                                        status_badge(
                                                            ui,
                                                            "Applied",
                                                            BadgeTone::Ready,
                                                        );
                                                    } else if install_dir.is_some() {
                                                        status_badge(ui, "Ready", BadgeTone::Blue);
                                                    } else {
                                                        status_badge(
                                                            ui,
                                                            "Manifest Only",
                                                            BadgeTone::Neutral,
                                                        );
                                                    }
                                                },
                                            );
                                            if !fx.description.trim().is_empty() {
                                                ui.add_space(4.0);
                                                ui.label(
                                                    RichText::new(fx.description.trim())
                                                        .size(12.5)
                                                        .color(Palette::TEXT_SECONDARY),
                                                );
                                            }
                                            ui.add_space(10.0);
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 8.0;
                                                if fx.has_manifest
                                                    && secondary_button(ui, "Install Manifest")
                                                        .clicked()
                                                {
                                                    act_mf = Some(fx.clone());
                                                }
                                                if fx.has_fix {
                                                    ui.add_enabled_ui(
                                                        install_dir.is_some(),
                                                        |ui| {
                                                            if primary_button(ui, "Apply Fix")
                                                                .clicked()
                                                            {
                                                                if let Some(dir) = &install_dir {
                                                                    act_fix = Some((
                                                                        fx.clone(),
                                                                        dir.clone(),
                                                                    ));
                                                                }
                                                            }
                                                        },
                                                    );
                                                }
                                                if applied {
                                                    if let Some(dir) = &install_dir {
                                                        if danger_button(ui, "Revert").clicked() {
                                                            act_rev =
                                                                Some((fx.id.clone(), dir.clone()));
                                                        }
                                                    }
                                                }
                                            });
                                        });
                                    }
                                });

                            if let Some(fx) = act_mf {
                                self.apply_fix_manifest_async(aid, fx, ui.ctx());
                            }
                            if let Some((fx, dir)) = act_fix {
                                self.apply_fix_files_async(aid, fx, dir, ui.ctx());
                            }
                            if let Some((fid, dir)) = act_rev {
                                let res = FixesService::unfix_game(&dir, &fid);
                                if res.ok {
                                    self.notify(
                                        format!(
                                            "Reverted {} file(s) for AppID {}",
                                            res.applied_files.len(),
                                            aid
                                        ),
                                        ToastKind::Success,
                                    );
                                } else {
                                    self.notify(
                                        res.error.unwrap_or_else(|| "Revert failed".into()),
                                        ToastKind::Warning,
                                    );
                                }
                            }
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space(70.0);
                                draw_rem_medallion(ui, 48.0);
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new("Select a game from the catalog")
                                        .font(display_font(17.0))
                                        .color(Palette::TEXT_PRIMARY),
                                );
                                ui.label(
                                    RichText::new(
                                        "All patched files are backed up with SHA-256 verification.",
                                    )
                                    .size(13.0)
                                    .color(Palette::TEXT_MUTED),
                                );
                            });
                        }

                        if self.fixes_busy {
                            ui.add_space(8.0);
                            ui.add(
                                ProgressBar::new(self.fixes_progress)
                                    .show_percentage()
                                    .corner_radius(CornerRadius::same(3)),
                            );
                        }
                    });
                });
            });

        if let Some(aid) = clicked_fix_game {
            self.open_app_in_fixes(aid, ui.ctx());
        }
    }

    // ===== 6. TABBED SETTINGS (CALM, UNCLUTTERED, FOCUSED) =====

    pub fn render_settings(&mut self, ui: &mut egui::Ui) {
        let mut save_all = false;
        section_header(
            ui,
            "SETTINGS",
            "Preferences & Integrations",
            "Configure Steam paths, cloud providers, SLSsteam, external tools, and Git updates",
            |ui| {
                if ghost_button(ui, "About").clicked() {
                    self.about_modal_open = true;
                }
                if primary_button(ui, "Save Changes").clicked() {
                    save_all = true;
                }
            },
        );

        if save_all {
            let sls_ok = self.slssteam_service.save();
            let app_ok = self.settings_service.save();
            self.telemetry_service
                .set_enabled(self.settings_service.settings.telemetry_enabled);
            self.telemetry_service.set_endpoint_url(
                self.settings_service
                    .settings
                    .telemetry_endpoint_url
                    .clone(),
            );
            if self.settings_service.settings.cef_bridge_enabled {
                self.cef_bridge.start();
            } else {
                self.cef_bridge.stop();
            }
            self.detect_steam_and_sync();
            let _ = self
                .play_unowned_service
                .sync_plugin_file(&self.slssteam_service);
            if sls_ok && app_ok {
                self.notify(
                    "Saved settings.json and SLSsteam config.yaml",
                    ToastKind::Success,
                );
            } else {
                self.notify(
                    "Failed to save one or more configuration files",
                    ToastKind::Warning,
                );
            }
        }

        // Clean Sub-Tab Strip
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if chip_button(
                ui,
                "General & Steam",
                self.settings_subtab == SettingsSubTab::General,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::General;
            }
            if chip_button(
                ui,
                "Account & Providers",
                self.settings_subtab == SettingsSubTab::Account,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::Account;
            }
            if chip_button(
                ui,
                "SLSsteam & Store Bridge",
                self.settings_subtab == SettingsSubTab::Integrations,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::Integrations;
            }
            if chip_button(
                ui,
                "Steam Integration",
                self.settings_subtab == SettingsSubTab::SteamIntegration,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::SteamIntegration;
            }
            if chip_button(
                ui,
                "Updates & Toolchain",
                self.settings_subtab == SettingsSubTab::UpdatesAndTools,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::UpdatesAndTools;
            }
            if chip_button(
                ui,
                "Telemetry & Logs",
                self.settings_subtab == SettingsSubTab::Telemetry,
            )
            .clicked()
            {
                self.settings_subtab = SettingsSubTab::Telemetry;
            }
        });

        ui.add_space(16.0);

        ScrollArea::vertical()
            .id_salt("settings_tab_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(820.0);
                match self.settings_subtab {
                    SettingsSubTab::General => self.render_settings_general(ui),
                    SettingsSubTab::Account => self.render_settings_account(ui),
                    SettingsSubTab::Integrations => self.render_settings_integrations(ui),
                    SettingsSubTab::SteamIntegration => self.render_settings_steam_integration(ui),
                    SettingsSubTab::UpdatesAndTools => self.render_settings_updates_and_tools(ui),
                    SettingsSubTab::Telemetry => self.render_settings_telemetry(ui),
                }
            });
    }

    fn render_settings_general(&mut self, ui: &mut egui::Ui) {
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Steam Installation & Manifest Behavior")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Leave the override empty to auto-detect ~/.local/share/Steam, ~/.steam/steam, or Flatpak Steam.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );

            ui.horizontal(|ui| {
                let w = (ui.available_width() - 100.0).max(200.0);
                ui.add_sized(
                    [w, 32.0],
                    TextEdit::singleline(&mut self.settings_service.settings.steam_path_override)
                        .margin(Margin::symmetric(10, 7))
                        .hint_text("Auto-detect Steam root path…"),
                );
                if secondary_button(ui, "Browse…").clicked() {
                    let tx = self.msg_tx.clone();
                    let ctx = ui.ctx().clone();
                    thread::spawn(move || {
                        if let Some(dir) = pick_directory_native("Select Steam Root Folder") {
                            let _ = tx.send(AppMessage::FolderPickedForSteamOverride(dir));
                            ctx.request_repaint();
                        }
                    });
                }
            });

            ui.add_space(4.0);
            ui.checkbox(
                &mut self.settings_service.settings.auto_update_apps,
                "Auto-update apps (comment out setManifestid pins on standard installs)",
            );
            ui.checkbox(
                &mut self.settings_service.settings.fast_fetch,
                "Enable FastFetch (select the first available manifest provider automatically)",
            );
            ui.checkbox(
                &mut self.settings_service.settings.donate_keys,
                "Contribute newly discovered depot decryption keys from Steam config.vdf",
            );

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if secondary_button(ui, "Rescan Steam").clicked() {
                    self.detect_steam_and_sync();
                    let _ = self
                        .play_unowned_service
                        .sync_plugin_file(&self.slssteam_service);
                }
                if secondary_button(ui, "Donate Keys Now").clicked() {
                    self.trigger_donate_keys(ui.ctx());
                }
                if ghost_button(ui, "Restart Steam").clicked() {
                    self.trigger_restart_steam(ui.ctx());
                }
            });
        });
    }

    fn render_settings_account(&mut self, ui: &mut egui::Ui) {
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("LuaTools Account (Discord)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if self.auth_service.is_signed_in() {
                        status_badge(ui, "Signed In", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "Guest", BadgeTone::Neutral);
                    }
                },
            );

            if self.auth_service.is_signed_in() {
                ui.label(
                    RichText::new(format!(
                        "{} ({})",
                        self.auth_service.display_name(),
                        self.auth_service.email()
                    ))
                    .font(semibold_font(13.5)),
                );
            } else {
                ui.label(
                    RichText::new("Sign in with Discord to download manifests from lua.tools.")
                        .size(12.5)
                        .color(Palette::TEXT_MUTED),
                );
            }
            if !self.settings_usage_text.is_empty() {
                ui.label(
                    RichText::new(&self.settings_usage_text)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if primary_button(ui, "Sign In with Browser").clicked() {
                    let auth = self.auth_service.clone();
                    let tx = self.msg_tx.clone();
                    let ctx = ui.ctx().clone();
                    self.status_text = "Waiting for Discord OAuth in browser…".into();
                    thread::spawn(move || {
                        match auth.sign_in_with_browser(180) {
                            Ok(s) => {
                                let _ = tx.send(AppMessage::AuthCompleted {
                                    ok: true,
                                    msg: format!("Signed in as {}", s.display_name),
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(AppMessage::AuthCompleted { ok: false, msg: e });
                            }
                        }
                        ctx.request_repaint();
                    });
                }
                if secondary_button(ui, "Use 6-Char Bot Code").clicked() {
                    self.text_prompt_modal.open = true;
                    self.text_prompt_modal.kind = "bot_code".into();
                    self.text_prompt_modal.title = "Discord Bot Code".into();
                    self.text_prompt_modal.prompt =
                        "Enter the 6-character code from Discord /login:".into();
                    self.text_prompt_modal.value.clear();
                }
                if self.auth_service.is_signed_in() && danger_button(ui, "Sign Out").clicked() {
                    self.auth_service.sign_out();
                    self.settings_usage_text.clear();
                    self.status_text = "Signed out.".into();
                }
            });
        });

        ui.add_space(16.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Hubcap Manifest Provider")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new("Optional API key from hubcapmanifest.com (smm_ + 96 hex chars).")
                    .size(12.5)
                    .color(Palette::TEXT_MUTED),
            );

            ui.horizontal(|ui| {
                let w = (ui.available_width() - 210.0).max(160.0);
                ui.add_sized(
                    [w, 32.0],
                    TextEdit::singleline(&mut self.settings_service.settings.hubcap_api_key)
                        .password(!self.settings_show_hubcap)
                        .hint_text("smm_…"),
                );
                if ghost_button(
                    ui,
                    if self.settings_show_hubcap {
                        "Hide"
                    } else {
                        "Show"
                    },
                )
                .clicked()
                {
                    self.settings_show_hubcap = !self.settings_show_hubcap;
                }
                if secondary_button(ui, "Verify Key").clicked() {
                    let key = self
                        .settings_service
                        .settings
                        .hubcap_api_key
                        .trim()
                        .to_string();
                    if !HubcapService::is_valid_key_format(&key) {
                        self.settings_hubcap_status =
                            "Invalid format — expected smm_ + 96 lowercase hex chars.".into();
                    } else {
                        let hc = self.hubcap_service.clone();
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        self.settings_hubcap_status = "Checking Hubcap quota…".into();
                        thread::spawn(move || {
                            let msg = if let Some(st) = hc.get_stats(&key) {
                                (
                                    true,
                                    format!(
                                        "Valid key · Daily usage: {}/{} · Active: {}",
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
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }
        });

        ui.add_space(16.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Steam DepotDownloader Session")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Official SteamRE/DepotDownloader authenticates with your Steam account for owned depots (-remember-password) or runs anonymously for public depots.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );
            let mut open_qr = false;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(
                    RichText::new("Steam Username:")
                        .size(12.5)
                        .color(Palette::TEXT_SECONDARY),
                );
                let w = (ui.available_width() - 210.0).max(140.0);
                ui.add_sized(
                    [w, 30.0],
                    TextEdit::singleline(&mut self.settings_service.settings.steam_username)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text("Optional (leave blank for anonymous / QR)"),
                );
                if secondary_button(ui, "Sign In via QR (Terminal)").clicked() {
                    open_qr = true;
                }
            });
            if open_qr {
                let _ = self.settings_service.save();
                let wp = self.slssteam_service.config.workshop_path.clone();
                let user = self.settings_service.settings.steam_username.clone();
                match self
                    .depot_downloader
                    .launch_interactive_login(Some(wp.as_str()), &user)
                {
                    Ok(msg) => self.notify(msg, ToastKind::Info),
                    Err(err) => self.notify(err, ToastKind::Warning),
                }
            }
        });

        ui.add_space(24.0);
    }

    fn render_settings_integrations(&mut self, ui: &mut egui::Ui) {
        let mut sync_play_unowned = false;
        let plugin_installed = self.play_unowned_service.is_installed();

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("SLSsteam Unlocker (~/.config/SLSsteam/config.yaml)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if plugin_installed {
                        status_badge(ui, "PlayUnowned .lua Synced", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "PlayUnowned .lua Inactive", BadgeTone::Muted);
                    }
                },
            );
            let cfg = &mut self.slssteam_service.config;
            ui.label(
                RichText::new(format!(
                    "{} AdditionalApps · {} FakeAppIds · {} DepotKeys · {} AppTokens",
                    cfg.additional_apps.len(),
                    cfg.fake_app_ids.len(),
                    cfg.depot_keys.len(),
                    cfg.app_tokens.len()
                ))
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );
            ui.checkbox(
                &mut cfg.play_not_owned,
                "Allow playing unowned games (PlayNotOwnedGames: true + play_unowned_games.lua)",
            );
            ui.checkbox(
                &mut cfg.limit_downloads,
                "Restrict downloads to Steam Library folders (LimitDownloadsToSteamLibrary)",
            );
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 100.0).max(180.0);
                ui.add_sized(
                    [w, 32.0],
                    TextEdit::singleline(&mut cfg.workshop_path)
                        .hint_text("Optional custom DepotDownloaderMod executable path…"),
                );
                if secondary_button(ui, "Browse…").clicked() {
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
            if secondary_button(ui, "Sync PlayUnowned Plugin Now").clicked() {
                sync_play_unowned = true;
            }
        });

        if sync_play_unowned {
            let enabled = self.slssteam_service.config.play_not_owned;
            match self
                .play_unowned_service
                .set_enabled(enabled, &mut self.slssteam_service)
            {
                Ok(()) => self.notify(
                    if enabled {
                        "Synced play_unowned_games.lua and SLSsteam config.yaml."
                    } else {
                        "Disabled PlayNotOwnedGames and removed play_unowned_games.lua."
                    },
                    ToastKind::Success,
                ),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }

        ui.add_space(16.0);

        // Native Wired Suite (ACCELA + SLSsteam) Installer Card
        let wired_st = self.wired_service.status();
        let mut wired_act: Option<&'static str> = None;
        let mut launch_accela = false;
        let mut uninstall_wired = false;

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Wired Suite (Native ACCELA & SLSsteam Installer)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if wired_st.slssteam_installed {
                        status_badge(ui, "SLSsteam.so Ready", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "SLSsteam Missing", BadgeTone::Muted);
                    }
                    if wired_st.accela_installed {
                        status_badge(ui, "ACCELA Ready", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "ACCELA Missing", BadgeTone::Muted);
                    }
                },
            );
            ui.label(
                RichText::new(
                    "Installs ACCELA (~/.local/share/ACCELA) and SLSsteam (~/.local/share/SLSsteam + ~/.local/bin/steam-sls LD_AUDIT launcher) natively in Rust without piping remote shell scripts.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_enabled_ui(!self.wired_busy, |ui| {
                    if primary_button(ui, "Install Both (Wired)").clicked() {
                        wired_act = Some("all");
                    }
                    if secondary_button(ui, "Install SLSsteam").clicked() {
                        wired_act = Some("slssteam");
                    }
                    if secondary_button(ui, "Install ACCELA").clicked() {
                        wired_act = Some("accela");
                    }
                    if wired_st.accela_installed && secondary_button(ui, "Launch ACCELA").clicked()
                    {
                        launch_accela = true;
                    }
                    if (wired_st.accela_installed || wired_st.slssteam_installed)
                        && danger_button(ui, "Uninstall Wired").clicked()
                    {
                        uninstall_wired = true;
                    }
                });
            });
            if self.wired_busy {
                ui.add(
                    ProgressBar::new(self.wired_progress)
                        .show_percentage()
                        .corner_radius(CornerRadius::same(3)),
                );
            }
            if !self.wired_status_text.is_empty() {
                ui.label(
                    RichText::new(&self.wired_status_text)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }
        });

        if let Some(act) = wired_act {
            self.trigger_wired_action(act, ui.ctx());
        }
        if launch_accela {
            match self.wired_service.launch_accela() {
                Ok(m) => self.notify(m, ToastKind::Info),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if uninstall_wired {
            match self.wired_service.uninstall_all() {
                Ok(()) => {
                    self.wired_status_text = "Uninstalled ACCELA and SLSsteam.".into();
                    self.notify("Uninstalled ACCELA and SLSsteam.", ToastKind::Info);
                }
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }

        ui.add_space(16.0);

        // Cloud Redirect Revival Card
        let cr_status = self.cloud_redirect_service.status();
        let cloud_apps = self.cloud_redirect_service.list_cloud_apps();
        let mut sync_cloud = false;
        let mut add_cloud_app: Option<u32> = None;
        let mut toggle_cloud: Option<(u32, bool)> = None;

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Cloud Redirect (Steam Cloud Revival)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| match &cr_status {
                    CloudRedirectStatus::Active => {
                        status_badge(ui, "CloudRedirect Active", BadgeTone::Ready);
                    }
                    CloudRedirectStatus::Installed => {
                        status_badge(ui, "Installed", BadgeTone::Blue);
                    }
                    CloudRedirectStatus::NotInstalled => {
                        status_badge(ui, "Not Installed", BadgeTone::Muted);
                    }
                    CloudRedirectStatus::Error(_) => {
                        status_badge(ui, "Error", BadgeTone::Warn);
                    }
                },
            );
            ui.label(
                RichText::new(format!(
                    "Config path: {}  ·  {} cloud-synced app(s)",
                    self.cloud_redirect_service.redirect_path().display(),
                    cloud_apps.len()
                ))
                .size(12.0)
                .color(Palette::TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                ui.add_sized(
                    [150.0, 28.0],
                    TextEdit::singleline(&mut self.cloud_app_input)
                        .margin(Margin::symmetric(8, 5))
                        .hint_text("AppID (e.g. 1245620)"),
                );
                if secondary_button(ui, "+ Enable Cloud App").clicked() {
                    if let Ok(aid) = self.cloud_app_input.trim().parse::<u32>() {
                        add_cloud_app = Some(aid);
                    }
                }
                if primary_button(ui, "Sync to CloudRedirect").clicked() {
                    sync_cloud = true;
                }
            });

            // Per-app Cloud toggles across Library + currently enabled Cloud apps
            let mut candidate_ids: Vec<(u32, String)> = self
                .library_rows
                .iter()
                .map(|r| (r.app_id, r.name.clone()))
                .collect();
            for &cid in &cloud_apps {
                if !candidate_ids.iter().any(|(id, _)| *id == cid) {
                    candidate_ids.push((cid, self.resolve_game_title(cid)));
                }
            }
            if !candidate_ids.is_empty() {
                inset_paper_sheet(ui, Margin::symmetric(10, 8), |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(14.0, 6.0);
                        for (aid, name) in candidate_ids.iter().take(12) {
                            let mut enabled = self.cloud_redirect_service.is_cloud_enabled(*aid);
                            let lbl = format!("{} (#{})", name, aid);
                            if ui.checkbox(&mut enabled, lbl).changed() {
                                toggle_cloud = Some((*aid, enabled));
                            }
                        }
                    });
                });
            }
        });

        if let Some(aid) = add_cloud_app {
            self.cloud_redirect_service.enable_cloud_for_app(aid);
            self.cloud_app_input.clear();
            self.notify(
                format!("Enabled CloudRedirect sync for AppID {}.", aid),
                ToastKind::Info,
            );
        }
        if let Some((aid, on)) = toggle_cloud {
            if on {
                self.cloud_redirect_service.enable_cloud_for_app(aid);
            } else {
                self.cloud_redirect_service.disable_cloud_for_app(aid);
            }
        }
        if sync_cloud {
            match self.cloud_redirect_service.sync_to_cloudredirect() {
                Ok(p) => self.notify(
                    format!("Synced CloudRedirect config to {}", p.display()),
                    ToastKind::Success,
                ),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }

        ui.add_space(16.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Steam Store Bridge (Millennium, CEF & URI Handler)")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(&self.settings_millennium_status)
                    .size(12.5)
                    .color(if self.millennium_connected {
                        Palette::GO_TEXT
                    } else {
                        Palette::TEXT_MUTED
                    }),
            );
            ui.checkbox(
                &mut self.settings_service.settings.cef_bridge_enabled,
                "Enable CEF DevTools Store Bridge (127.0.0.1:8080 › local RPC :6767)",
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if secondary_button(ui, "Probe Millennium").clicked() {
                    self.probe_bridge_async(ui.ctx());
                }
                if secondary_button(ui, "Enable Steam CEF Debugging").clicked() {
                    if let Some(inst) = &self.steam_service.installation {
                        match CefBridgeService::ensure_cef_debugging_enabled(&inst.root) {
                            Ok(m) => self.notify(m, ToastKind::Success),
                            Err(m) => self.notify(m, ToastKind::Warning),
                        }
                    } else {
                        self.notify("Steam installation not detected.", ToastKind::Warning);
                    }
                }
                if secondary_button(ui, "Register luatools:// Handler").clicked() {
                    match ProtocolService::register_desktop_handler() {
                        Ok(m) => self.notify(m, ToastKind::Success),
                        Err(m) => self.notify(m, ToastKind::Warning),
                    }
                }
            });
        });
        ui.add_space(24.0);
    }

    fn render_settings_updates_and_tools(&mut self, ui: &mut egui::Ui) {
        // 1. Git Self-Updater Card
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Application Self-Updater (Git)")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(&self.settings_update_status)
                    .size(12.5)
                    .color(Palette::BLUE_LIGHT),
            );
            ui.checkbox(
                &mut self.settings_service.settings.check_updates_on_startup,
                "Check Git remote once in the background on startup (shows ⬆ Update button in top-right corner)",
            );

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Remote URL:")
                        .size(12.5)
                        .color(Palette::TEXT_SECONDARY),
                );
                let w = (ui.available_width() - 12.0).max(180.0);
                ui.add_sized(
                    [w, 30.0],
                    TextEdit::singleline(&mut self.settings_service.settings.git_remote_url)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text("https://github.com/subsurfmod/LuaToolsLinux.git (default)"),
                );
            });
            let repo_hint = UpdateService::discover_repo_dir("")
                .map(|p| format!("Auto-detected ({})", p.display()))
                .unwrap_or_else(|| "Auto-detected (~/.local/share/luatools/repo)".into());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Local Repo:")
                        .size(12.5)
                        .color(Palette::TEXT_SECONDARY),
                );
                let w = (ui.available_width() - 100.0).max(160.0);
                ui.add_sized(
                    [w, 30.0],
                    TextEdit::singleline(&mut self.settings_service.settings.update_repo_dir)
                        .margin(Margin::symmetric(10, 6))
                        .hint_text(repo_hint),
                );
                if secondary_button(ui, "Browse…").clicked() {
                    let tx = self.msg_tx.clone();
                    let ctx = ui.ctx().clone();
                    thread::spawn(move || {
                        if let Some(dir) = pick_directory_native("Select Local Git Repository") {
                            let _ = tx.send(AppMessage::FolderPickedForUpdateRepo(dir));
                            ctx.request_repaint();
                        }
                    });
                }
            });

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if secondary_button(ui, "Check for Updates").clicked() {
                    let _ = self.settings_service.save();
                    self.check_app_update_once_async(ui.ctx());
                }
                if primary_button(ui, "Pull & Update Now").clicked() {
                    let _ = self.settings_service.save();
                    self.trigger_git_self_update(ui.ctx());
                }
            });
        });

        ui.add_space(18.0);
        ui.label(
            RichText::new("External Toolchain")
                .font(display_font(17.5))
                .color(Palette::TEXT_PRIMARY),
        );
        ui.add_space(8.0);

        for spec in TOOLS {
            let info = self.tools_manager.inspect_tool(spec.key);
            let is_busy = self.tool_busy.get(spec.key).copied().unwrap_or(false);
            let pct = self.tool_progress.get(spec.key).copied().unwrap_or(0.0);

            paper_card_padded(ui, Margin::same(16), |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(spec.name)
                                    .font(semibold_font(14.5))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(format!("github.com/{}", spec.github_repo))
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(Palette::TEXT_MUTED),
                            );
                        });
                    },
                    |ui| {
                        if info.installed {
                            status_badge(
                                ui,
                                &format!("Installed · {}", info.version),
                                BadgeTone::Ready,
                            );
                        } else {
                            status_badge(ui, "Not Installed", BadgeTone::Muted);
                        }
                    },
                );

                ui.label(
                    RichText::new(spec.description)
                        .size(12.5)
                        .color(Palette::TEXT_SECONDARY),
                );

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_enabled_ui(!is_busy, |ui| {
                        if primary_button(ui, "Install / Update").clicked() {
                            self.update_external_tool_async(spec.key, ui.ctx());
                        }
                    });
                    if ghost_button(ui, "Open Folder").clicked() {
                        let d = self.tools_manager.tools_dir.join(spec.key);
                        let _ = std::fs::create_dir_all(&d);
                        let _ = Command::new("xdg-open").arg(&d).spawn();
                    }
                    if spec.key == "steamautopatch"
                        && secondary_button(ui, "Run on Game Folder…").clicked()
                    {
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        thread::spawn(move || {
                            if let Some(dir) =
                                pick_directory_native("Select Game Directory for SteamAutoPatch")
                            {
                                let _ = tx.send(AppMessage::FolderPickedForAutoCrack(dir));
                                ctx.request_repaint();
                            }
                        });
                    }
                    if spec.key == "stub_remover" && secondary_button(ui, "Unpack .exe…").clicked()
                    {
                        let tx = self.msg_tx.clone();
                        let ctx = ui.ctx().clone();
                        thread::spawn(move || {
                            let files =
                                pick_files_native("Select Executable (.exe)", Some("*.exe"));
                            if let Some(f) = files.into_iter().next() {
                                let _ = tx.send(AppMessage::FilePickedForStubRemover(f));
                                ctx.request_repaint();
                            }
                        });
                    }
                });

                if is_busy {
                    ui.add(
                        ProgressBar::new(pct)
                            .show_percentage()
                            .corner_radius(CornerRadius::same(3)),
                    );
                }
            });
            ui.add_space(10.0);
        }
    }

    fn render_settings_telemetry(&mut self, ui: &mut egui::Ui) {
        let mut toggled_telemetry = false;
        let mut clear_logs = false;
        let mut reopen_notice = false;
        let mut copy_md = false;
        let mut export_md = false;
        let mut open_gh = false;
        let mut test_endpoint = false;

        let ev_count = self.telemetry_service.event_count();

        // 1. Master Telemetry Toggle & Privacy Controls
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Bug-Fixing Telemetry & Privacy Controls")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if self.settings_service.settings.telemetry_enabled {
                        status_badge(
                            ui,
                            &format!("Active · {} event(s)", ev_count),
                            BadgeTone::Ready,
                        );
                    } else {
                        status_badge(ui, "Turned Off", BadgeTone::Muted);
                    }
                },
            );

            ui.label(
                RichText::new(
                    "Records scrubbed runtime errors, crash locations, and anonymous Linux/display environment details \
                     in ~/.local/share/luatools/telemetry/events.jsonl. Usernames, Steam credentials, Discord tokens, \
                     Hubcap keys, and 64-character depot decryption keys are scrubbed automatically.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );

            if ui
                .checkbox(
                    &mut self.settings_service.settings.telemetry_enabled,
                    "Enable anonymous diagnostic & crash telemetry for bug fixing",
                )
                .changed()
            {
                toggled_telemetry = true;
            }

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if ghost_button(ui, "Review Privacy Popup").clicked() {
                    reopen_notice = true;
                }
                if danger_button(ui, "Clear Collected Data").clicked() {
                    clear_logs = true;
                }
            });
        });

        if toggled_telemetry {
            let enabled = self.settings_service.settings.telemetry_enabled;
            self.settings_service.settings.telemetry_consent_seen = true;
            if enabled
                && self
                    .settings_service
                    .settings
                    .telemetry_install_id
                    .trim()
                    .is_empty()
            {
                self.settings_service.settings.telemetry_install_id =
                    crate::services::telemetry_service::TelemetryService::generate_install_id();
            } else if !enabled {
                self.settings_service.settings.telemetry_install_id.clear();
            }
            let _ = self.settings_service.save();
            self.telemetry_service.set_enabled(enabled);
            if enabled {
                self.telemetry_service.record(
                    "info",
                    "telemetry",
                    "telemetry_enabled",
                    "Telemetry enabled in Settings",
                );
                self.notify("Diagnostic telemetry enabled.", ToastKind::Success);
            } else {
                self.notify(
                    "Telemetry turned off — no new diagnostic events will be collected.",
                    ToastKind::Info,
                );
            }
        }
        if reopen_notice {
            self.telemetry_modal_open = true;
        }
        if clear_logs {
            self.telemetry_service.clear_all();
            self.notify(
                "Cleared all locally stored telemetry events.",
                ToastKind::Info,
            );
        }

        ui.add_space(16.0);

        // 2. 1-Click Bug Report & Diagnostic Bundle Exporter
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Share Diagnostics for Bug Fixing")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Generate a clean, PII-scrubbed Markdown summary of your Linux environment and recent errors to attach to a bug report.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if primary_button(ui, "Report Bug on GitHub ›").clicked() {
                    open_gh = true;
                }
                if secondary_button(ui, "Copy Diagnostic Markdown").clicked() {
                    copy_md = true;
                }
                if secondary_button(ui, "Export Report File…").clicked() {
                    export_md = true;
                }
            });
        });

        if open_gh {
            match self.telemetry_service.open_github_bug_report() {
                Ok(m) => self.notify(m, ToastKind::Info),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if copy_md {
            let md = self.telemetry_service.build_diagnostic_markdown(60);
            ui.ctx().copy_text(md);
            self.notify(
                "Copied scrubbed diagnostic report to clipboard.",
                ToastKind::Success,
            );
        }
        if export_md {
            let tx = self.msg_tx.clone();
            let ctx = ui.ctx().clone();
            thread::spawn(move || {
                if let Some(dir) = pick_directory_native("Select Folder for Diagnostic Report") {
                    let _ = tx.send(AppMessage::FolderPickedForDiagExport(dir));
                    ctx.request_repaint();
                }
            });
        }

        ui.add_space(16.0);

        // 3. Optional Remote Collector / Webhook Endpoint
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(
                RichText::new("Optional Remote Telemetry Collector")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Leave blank to keep telemetry 100% local, or configure an HTTPS JSON collector / Discord webhook URL to receive scrubbed bug events automatically when telemetry is enabled.",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 140.0).max(180.0);
                ui.add_sized(
                    [w, 30.0],
                    TextEdit::singleline(
                        &mut self.settings_service.settings.telemetry_endpoint_url,
                    )
                    .margin(Margin::symmetric(10, 6))
                    .hint_text("https://... (optional JSON endpoint or Discord webhook)"),
                );
                if secondary_button(ui, "Test Endpoint").clicked() {
                    test_endpoint = true;
                }
            });
            if !self.settings_telemetry_status.is_empty() {
                ui.label(
                    RichText::new(&self.settings_telemetry_status)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }
        });

        if test_endpoint {
            let _ = self.settings_service.save();
            self.telemetry_service.set_endpoint_url(
                self.settings_service
                    .settings
                    .telemetry_endpoint_url
                    .clone(),
            );
            let tel = self.telemetry_service.clone();
            let tx = self.msg_tx.clone();
            let ctx = ui.ctx().clone();
            self.settings_telemetry_status = "Sending test diagnostic ping…".into();
            thread::spawn(move || {
                match tel.send_test_ping() {
                    Ok(m) => {
                        let _ = tx.send(AppMessage::TelemetryEndpointTested { ok: true, msg: m });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMessage::TelemetryEndpointTested { ok: false, msg: e });
                    }
                }
                ctx.request_repaint();
            });
        }

        ui.add_space(16.0);

        // 4. Transparent Local Event Log Viewer
        let events = self.telemetry_service.recent_events(35);
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("Recorded Telemetry Events (Newest First)")
                    .font(display_font(17.0))
                    .color(Palette::TEXT_PRIMARY),
            );
            inset_paper_sheet(ui, Margin::same(10), |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(130.0);
                if events.is_empty() {
                    ui.label(
                        RichText::new("No telemetry events currently stored.")
                            .size(12.5)
                            .color(Palette::TEXT_MUTED),
                    );
                } else {
                    ScrollArea::vertical()
                        .id_salt("telemetry_events_viewer_scroll")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            TableBuilder::new(ui)
                                .striped(true)
                                .cell_layout(Layout::left_to_right(Align::Center))
                                .column(Column::exact(145.0))
                                .column(Column::exact(72.0))
                                .column(Column::exact(96.0))
                                .column(Column::remainder().at_least(220.0))
                                .header(26.0, |mut h| {
                                    for lbl in ["Timestamp (UTC)", "Level", "Category", "Event"] {
                                        h.col(|ui| {
                                            ui.label(
                                                RichText::new(lbl)
                                                    .font(semibold_font(11.5))
                                                    .color(Palette::TEXT_MUTED),
                                            );
                                        });
                                    }
                                })
                                .body(|mut body| {
                                    for ev in &events {
                                        body.row(30.0, |mut row| {
                                            row.col(|ui| {
                                                let short_ts = ev
                                                    .timestamp
                                                    .get(0..19)
                                                    .unwrap_or(&ev.timestamp)
                                                    .replace('T', " ");
                                                ui.label(
                                                    RichText::new(short_ts)
                                                        .font(FontId::new(
                                                            11.0,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(Palette::TEXT_MUTED),
                                                );
                                            });
                                            row.col(|ui| {
                                                let tone = match ev.level.as_str() {
                                                    "panic" | "error" => BadgeTone::Danger,
                                                    "warn" => BadgeTone::Warn,
                                                    _ => BadgeTone::Blue,
                                                };
                                                status_badge(
                                                    ui,
                                                    &ev.level.to_ascii_uppercase(),
                                                    tone,
                                                );
                                            });
                                            row.col(|ui| {
                                                ui.label(
                                                    RichText::new(&ev.category)
                                                        .font(FontId::new(
                                                            11.5,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(Palette::TEXT_SECONDARY),
                                                );
                                            });
                                            row.col(|ui| {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} — {}",
                                                        ev.event, ev.message
                                                    ))
                                                    .size(12.0)
                                                    .color(Palette::TEXT_PRIMARY),
                                                );
                                            });
                                        });
                                    }
                                });
                        });
                }
            });
        });

        ui.add_space(24.0);
    }
}

impl LuaToolsApp {
    fn render_settings_steam_integration(&mut self, ui: &mut egui::Ui) {
        let st = self.steam_bridge_service.status();
        let mut do_install = false;
        let mut do_uninstall = false;
        let mut do_scan = false;
        let mut do_refresh = false;
        let mut toggle_hr = false;

        // 1. Steam Detection, Integration Library & LD_PRELOAD Mode
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Steam Client Interop (libsteam_integration.so)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if st.integration_active {
                        status_badge(ui, "IPC Active in Steam", BadgeTone::Ready);
                    } else if st.integration_lib_installed {
                        status_badge(ui, "Library Staged", BadgeTone::Blue);
                    } else {
                        status_badge(ui, "Not Installed", BadgeTone::Muted);
                    }
                    if st.steam_detected {
                        status_badge(ui, "Steam Detected", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "No Steam", BadgeTone::Warn);
                    }
                },
            );

            let root_txt = st
                .steam_root
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Not detected".into());
            let pid_txt = st
                .steam_pid
                .map(|p| format!("Running (PID {})", p))
                .unwrap_or_else(|| "Not running".into());
            ui.label(
                RichText::new(format!(
                    "Steam root: {}  ·  Process: {}  ·  Ticketed apps: {}",
                    root_txt,
                    pid_txt,
                    st.apps_with_tickets.len()
                ))
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );

            ui.checkbox(
                &mut self.bridge_system_preload,
                "Enable system-wide LD_PRELOAD in /etc/ld.so.preload (requires administrator/root; optional — per-launch launcher works without it)",
            );
            ui.label(
                RichText::new("Recommended: leave unchecked. Use ~/.local/bin/luatools-steam-launcher to start Steam instead.")
                    .size(11.0)
                    .color(Palette::TEXT_MUTED),
            );

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_enabled_ui(!self.bridge_busy, |ui| {
                    let install_lbl = if self.bridge_system_preload {
                        "Install Integration (System Preload)"
                    } else {
                        "Install Integration (No Root)"
                    };
                    if primary_button(ui, install_lbl).clicked() {
                        do_install = true;
                    }
                    if (st.integration_lib_installed
                        || st.launcher_installed
                        || st.system_preload_enabled)
                        && danger_button(ui, "Uninstall Integration").clicked()
                    {
                        do_uninstall = true;
                    }
                    if secondary_button(ui, "Refresh Status").clicked() {
                        do_refresh = true;
                    }
                });
            });

            if !self.bridge_status_msg.is_empty() {
                ui.label(
                    RichText::new(&self.bridge_status_msg)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }

            // Progress bar for installation
            if st.install_progress > 0.0 && st.install_progress < 100.0 {
                ui.add_space(4.0);
                egui::ProgressBar::new(st.install_progress / 100.0)
                    .text(format!("{}%", st.install_progress as u32))
                    .animate(true);
                ui.label(
                    RichText::new(&st.install_status)
                        .size(11.0)
                        .color(Palette::TEXT_SECONDARY),
                );
                ui.add_space(4.0);
            } else if st.install_progress >= 100.0 && !st.install_status.is_empty() {
                ui.label(
                    RichText::new(&st.install_status)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }
        });

        if do_refresh {
            self.steam_bridge_service.refresh_steam_detection();
            self.bridge_status_msg = "Refreshed Steam process & IPC detection.".into();
        }
        if do_install {
            self.trigger_bridge_action_async("install", ui.ctx());
        }
        if do_uninstall {
            self.trigger_bridge_action_async("uninstall", ui.ctx());
        }

        ui.add_space(16.0);

        // 2. ELF64 Binary & Pattern Database Status
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("ELF64 Binary & Pattern Tracker Status")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if st.patterns_loaded {
                        status_badge(ui, "Patterns Matched", BadgeTone::Ready);
                    } else if st.steam_client_found || st.steam_ui_found {
                        status_badge(ui, "ELF Scanned", BadgeTone::Blue);
                    } else {
                        status_badge(ui, "Not Scanned", BadgeTone::Muted);
                    }
                },
            );

            inset_paper_sheet(ui, Margin::symmetric(12, 10), |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                let sc_sha = st.steam_client_sha256.as_deref().unwrap_or("—");
                let sc_pat = st.steam_client_patterns.as_deref().unwrap_or("None loaded");
                let su_sha = st.steam_ui_sha256.as_deref().unwrap_or("—");
                let su_pat = st.steam_ui_patterns.as_deref().unwrap_or("None loaded");

                ui.label(
                    RichText::new(format!("steamclient.so SHA-256: {}", sc_sha))
                        .font(FontId::new(11.5, FontFamily::Monospace))
                        .color(Palette::TEXT_SECONDARY),
                );
                ui.label(
                    RichText::new(format!("  Matched patterns: {}", sc_pat))
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("steamui.so     SHA-256: {}", su_sha))
                        .font(FontId::new(11.5, FontFamily::Monospace))
                        .color(Palette::TEXT_SECONDARY),
                );
                ui.label(
                    RichText::new(format!("  Matched patterns: {}", su_pat))
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            });

            ui.add_enabled_ui(!self.bridge_busy, |ui| {
                if secondary_button(ui, "Scan Binaries & Load Patterns").clicked() {
                    do_scan = true;
                }
            });
        });

        if do_scan {
            self.trigger_bridge_action_async("scan", ui.ctx());
        }

        ui.add_space(16.0);

        // 3. Hot-Reload Service (inotify stplug-in/ watcher + event log)
        let hr_running = self.hot_reload_service.is_running();
        let hr_events = self.hot_reload_service.recent_events();

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("Live stplug-in/ Hot-Reload Watcher (inotify)")
                            .font(display_font(17.5))
                            .color(Palette::TEXT_PRIMARY),
                    );
                },
                |ui| {
                    if hr_running {
                        status_badge(ui, "Watcher Running", BadgeTone::Ready);
                    } else {
                        status_badge(ui, "Watcher Stopped", BadgeTone::Muted);
                    }
                },
            );

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let btn_lbl = if hr_running {
                    "Stop Hot-Reload Watcher"
                } else {
                    "Start Hot-Reload Watcher"
                };
                if secondary_button(ui, btn_lbl).clicked() {
                    toggle_hr = true;
                }
                ui.label(
                    RichText::new(format!(
                        "Watching: {}",
                        self.hot_reload_service.watch_dir().display()
                    ))
                    .size(12.0)
                    .color(Palette::TEXT_MUTED),
                );
            });

            inset_paper_sheet(ui, Margin::same(10), |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(96.0);
                if hr_events.is_empty() {
                    ui.label(
                        RichText::new("No .lua file events observed in this session yet.")
                            .size(12.0)
                            .color(Palette::TEXT_MUTED),
                    );
                } else {
                    ScrollArea::vertical()
                        .id_salt("hot_reload_events_scroll")
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for ev in &hr_events {
                                ui.label(
                                    RichText::new(format!(
                                        "[{}] {:<8} {}",
                                        ev.timestamp,
                                        ev.event_type.as_str(),
                                        ev.path.display()
                                    ))
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(Palette::TEXT_SECONDARY),
                                );
                            }
                        });
                }
            });
        });

        if toggle_hr {
            if hr_running {
                self.hot_reload_service.stop();
                self.notify("Stopped stplug-in/ hot-reload watcher.", ToastKind::Info);
            } else {
                match self.hot_reload_service.start() {
                    Ok(()) => {
                        self.notify("Started stplug-in/ hot-reload watcher.", ToastKind::Success)
                    }
                    Err(e) => self.notify(e, ToastKind::Warning),
                }
            }
        }

        ui.add_space(24.0);
    }
}
