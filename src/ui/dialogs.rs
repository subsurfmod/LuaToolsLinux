//! Modal dialogs (`AddByAppIdModal`, `GameDetailModal`, `LaunchOptionsModal`,
//! `DepotDetailModal`, and `TextPromptModal`) rendered with sharp 0px styling.

use std::path::PathBuf;
use std::thread;

use eframe::egui::{self, Align, ComboBox, Layout, RichText, ScrollArea, TextEdit};

use crate::parsers::vdf_binary::{LaunchEntry, LaunchOptionsService};
use crate::services::depot_downloader::{DepotDownloaderService, DepotSelection};
use crate::ui::app::LuaToolsApp;
use crate::ui::theme::{
    danger_button, format_bytes, ghost_button, lua_layouter, panel_frame, primary_button,
    secondary_button, section_header, Palette,
};

impl LuaToolsApp {
    pub fn render_modals(&mut self, ctx: &egui::Context) {
        self.render_add_by_appid_modal(ctx);
        self.render_game_detail_modal(ctx);
        self.render_launch_options_modal(ctx);
        self.render_depot_detail_modal(ctx);
        self.render_text_prompt_modal(ctx);
        self.render_about_modal(ctx);
    }

    fn render_add_by_appid_modal(&mut self, ctx: &egui::Context) {
        if !self.add_appid_modal.open {
            return;
        }
        let mut close = false;
        let mut create = false;

        egui::Window::new("CREATE STPLUG-IN LUA BY APPID")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([440.0, 230.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                egui::Grid::new("add_appid_grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label("AppID:");
                        ui.add(
                            TextEdit::singleline(&mut self.add_appid_modal.app_id_input)
                                .hint_text("e.g. 1245620")
                                .desired_width(300.0),
                        );
                        ui.end_row();

                        ui.label("Title:");
                        ui.add(
                            TextEdit::singleline(&mut self.add_appid_modal.title_input)
                                .hint_text("Optional game title comment")
                                .desired_width(300.0),
                        );
                        ui.end_row();

                        ui.label("Template:");
                        let templates = [
                            "Base App Only (addappid)",
                            "Base App + DLC Placeholder",
                            "Base App + Keyed Depot + Pinned Manifest",
                        ];
                        ComboBox::from_id_salt("tpl_combo")
                            .width(300.0)
                            .selected_text(templates[self.add_appid_modal.template_idx])
                            .show_ui(ui, |ui| {
                                for (i, t) in templates.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut self.add_appid_modal.template_idx,
                                        i,
                                        *t,
                                    );
                                }
                            });
                        ui.end_row();
                    });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "CREATE LUA").clicked() {
                        create = true;
                    }
                    if ghost_button(ui, "CANCEL").clicked() {
                        close = true;
                    }
                });
            });

        if create {
            if let Ok(aid) = self.add_appid_modal.app_id_input.trim().parse::<u32>() {
                let gname = self.add_appid_modal.title_input.trim();
                let comment = if gname.is_empty() {
                    String::new()
                } else {
                    format!(" -- {}", gname)
                };
                let content = match self.add_appid_modal.template_idx {
                    0 => format!("addappid({}){}\n", aid, comment),
                    1 => format!(
                        "addappid({}){}\n\n-- DLC entries:\n-- addappid({})\n",
                        aid,
                        comment,
                        aid + 10
                    ),
                    _ => format!(
                        "addappid({}){}\naddappid({}, 1, \"{}\")\nsetManifestid({}, \"0000000000000000000\", 0)\n",
                        aid,
                        comment,
                        aid + 1,
                        "0".repeat(64),
                        aid + 1
                    ),
                };
                if let Some(installer) = self.lua_installer.clone() {
                    let res = installer.install_lua(
                        aid,
                        &content,
                        true,
                        None,
                        Some("manual"),
                        Some(&mut self.settings_service),
                        Some(&mut self.slssteam_service),
                    );
                    if res.ok {
                        self.status_text = format!(
                            "Created {}.lua in stplug-in and synced Vault & SLSsteam",
                            aid
                        );
                        self.refresh_all_local_state();
                        close = true;
                    } else {
                        self.status_text = res.error.unwrap_or_else(|| "Failed".into());
                    }
                }
            } else {
                self.status_text = "Invalid AppID — enter a positive integer.".into();
            }
        }
        if close {
            self.add_appid_modal.open = false;
        }
    }

    fn render_game_detail_modal(&mut self, ctx: &egui::Context) {
        if !self.game_detail_modal.open {
            return;
        }
        let mut close = false;
        let mut do_save_lua = false;
        let mut do_del_lua = false;
        let mut do_set_fake = false;
        let mut do_rem_fake = false;
        let mut do_set_tok = false;
        let mut do_rem_tok = false;
        let mut do_pull_depot = false;

        let app_id = self.game_detail_modal.app_id;
        let title = format!(
            "GAME CONFIGURATION // {} ({})",
            self.game_detail_modal.game_name, app_id
        );

        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 620.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                section_header(
                    ui,
                    &app_id.to_string(),
                    &self.game_detail_modal.game_name,
                    "Direct stplug-in .lua manifest, SLSsteam overrides, and single-depot pull",
                    |_ui| {},
                );
                ui.add_space(6.0);

                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("STPLUG-IN LUA MANIFEST")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::BLUE_LIGHT),
                    );
                    let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                        lua_layouter(ui, string, wrap_width)
                    };
                    ScrollArea::vertical()
                        .id_salt("gd_lua_scroll")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            ui.add(
                                TextEdit::multiline(&mut self.game_detail_modal.lua_text)
                                    .code_editor()
                                    .desired_rows(8)
                                    .desired_width(f32::INFINITY)
                                    .layouter(&mut layouter),
                            );
                        });
                    ui.horizontal(|ui| {
                        if primary_button(ui, "SAVE LUA FILE").clicked() {
                            do_save_lua = true;
                        }
                        if danger_button(ui, "DELETE LUA FILE").clicked() {
                            do_del_lua = true;
                        }
                    });
                });

                ui.add_space(6.0);
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("FAKEAPPID OVERRIDE (SLSSTEAM)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Target App/Depot ID:");
                        ui.add(
                            TextEdit::singleline(&mut self.game_detail_modal.fake_id_input)
                                .hint_text("480")
                                .desired_width(110.0),
                        );
                        if secondary_button(ui, "SET FAKEAPPID").clicked() {
                            do_set_fake = true;
                        }
                        if danger_button(ui, "REMOVE").clicked() {
                            do_rem_fake = true;
                        }
                    });
                });

                ui.add_space(6.0);
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("APP ACCESS TOKEN (SLSSTEAM)")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.game_detail_modal.token_input)
                                .password(!self.game_detail_modal.show_token)
                                .hint_text("Enter Steam app access token...")
                                .desired_width(360.0),
                        );
                        let toggle_lbl = if self.game_detail_modal.show_token {
                            "HIDE"
                        } else {
                            "SHOW"
                        };
                        if ghost_button(ui, toggle_lbl).clicked() {
                            self.game_detail_modal.show_token = !self.game_detail_modal.show_token;
                        }
                        if secondary_button(ui, "SAVE TOKEN").clicked() {
                            do_set_tok = true;
                        }
                        if danger_button(ui, "CLEAR").clicked() {
                            do_rem_tok = true;
                        }
                    });
                });

                ui.add_space(6.0);
                panel_frame().show(ui, |ui| {
                    ui.label(
                        RichText::new("QUICK DEPOT PULL")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_SECONDARY),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Depot ID:");
                        ui.add(
                            TextEdit::singleline(&mut self.game_detail_modal.quick_depot_input)
                                .hint_text(format!("e.g. {}", app_id + 1))
                                .desired_width(130.0),
                        );
                        if secondary_button(ui, "DOWNLOAD DEPOT").clicked() {
                            do_pull_depot = true;
                        }
                    });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if !self.game_detail_modal.feedback.is_empty() {
                        ui.label(
                            RichText::new(&self.game_detail_modal.feedback)
                                .monospace()
                                .size(11.0)
                                .color(Palette::BLUE_LIGHT),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary_button(ui, "CLOSE").clicked() {
                            close = true;
                        }
                    });
                });
            });

        if do_save_lua {
            if let Some(installer) = self.lua_installer.clone() {
                let text = self.game_detail_modal.lua_text.clone();
                let res = installer.install_lua(
                    app_id,
                    &text,
                    true,
                    None,
                    Some("detail-modal"),
                    Some(&mut self.settings_service),
                    Some(&mut self.slssteam_service),
                );
                if res.ok {
                    self.game_detail_modal.feedback =
                        "SAVED :: Lua file written and synced with Vault & SLSsteam.".into();
                    self.refresh_all_local_state();
                } else {
                    self.game_detail_modal.feedback =
                        res.error.unwrap_or_else(|| "Save failed.".into());
                }
            }
        }
        if do_del_lua {
            if let Some(installer) = self.lua_installer.clone() {
                let res = installer.uninstall_lua(app_id, false, Some(&mut self.slssteam_service));
                if res.ok {
                    self.game_detail_modal.lua_text.clear();
                    self.game_detail_modal.feedback = "DELETED :: stplug-in .lua removed.".into();
                    self.refresh_all_local_state();
                }
            }
        }
        if do_set_fake {
            if let Ok(did) = self.game_detail_modal.fake_id_input.trim().parse::<u32>() {
                self.slssteam_service.set_fake_app_id(app_id, did);
                let _ = self.slssteam_service.save();
                self.game_detail_modal.feedback =
                    format!("SAVED :: FakeAppId {} set for AppID {}", did, app_id);
                self.refresh_library_rows();
            }
        }
        if do_rem_fake {
            self.slssteam_service.remove_fake_app_id(app_id);
            let _ = self.slssteam_service.save();
            self.game_detail_modal.fake_id_input.clear();
            self.game_detail_modal.feedback = "REMOVED :: FakeAppId cleared.".into();
            self.refresh_library_rows();
        }
        if do_set_tok {
            let tok = self.game_detail_modal.token_input.trim().to_string();
            if !tok.is_empty() {
                self.slssteam_service.set_token(app_id, tok);
                let _ = self.slssteam_service.save();
                self.game_detail_modal.feedback = "SAVED :: Access token stored.".into();
                self.refresh_library_rows();
            }
        }
        if do_rem_tok {
            self.slssteam_service.remove_token(app_id);
            let _ = self.slssteam_service.save();
            self.game_detail_modal.token_input.clear();
            self.game_detail_modal.feedback = "REMOVED :: Access token cleared.".into();
            self.refresh_library_rows();
        }
        if do_pull_depot {
            if let Ok(depot_id) = self
                .game_detail_modal
                .quick_depot_input
                .trim()
                .parse::<u32>()
            {
                let keys = DepotDownloaderService::resolve_keys(
                    self.steam_service.installation.as_ref(),
                    &self.slssteam_service,
                    app_id,
                );
                let dest = dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("LuaTools Depots")
                    .join(format!("depot_{}", depot_id));
                let dd = self.depot_downloader.clone();
                let wp = self.slssteam_service.config.workshop_path.clone();
                let tx = self.msg_tx.clone();
                let ctx_clone = ctx.clone();
                self.game_detail_modal.feedback = format!(
                    "STARTED :: Downloading depot {} -> {}",
                    depot_id,
                    dest.display()
                );
                thread::spawn(move || {
                    let sel = DepotSelection {
                        depot_id,
                        manifest_id: None,
                        manifest_path: None,
                        size: 0,
                        from_app_id: None,
                        language: None,
                        os: None,
                        is_shared: false,
                        is_dlc: false,
                    };
                    let res = dd.download_depot(app_id, &sel, &dest, &keys, Some(&wp), |_, _| {});
                    let _ = tx.send(crate::ui::state::AppMessage::DepotDownloadFinished {
                        results: vec![res],
                    });
                    ctx_clone.request_repaint();
                });
            }
        }
        if close {
            self.game_detail_modal.open = false;
        }
    }

    fn render_launch_options_modal(&mut self, ctx: &egui::Context) {
        if !self.launch_options_modal.open {
            return;
        }
        let mut close = false;
        let mut do_save = false;
        let mut do_restore = false;

        let app_id = self.launch_options_modal.app_id;
        let title = format!(
            "LAUNCH OPTIONS // {} ({})",
            self.launch_options_modal.game_name, app_id
        );

        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_size([700.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Palette::WARN_BG)
                    .stroke(egui::Stroke::new(1.0_f32, Palette::WARN_BORDER))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(
                                "CAUTION :: Close Steam completely before saving changes to appinfo.vdf so Steam does not overwrite them on shutdown.",
                            )
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::WARN_TEXT),
                        );
                    });

                ui.add_space(8.0);
                ui.columns(2, |cols| {
                    cols[0].label(
                        RichText::new("LAUNCH ENTRIES")
                            .monospace()
                            .strong()
                            .size(11.0)
                            .color(Palette::TEXT_MUTED),
                    );
                    ScrollArea::vertical()
                        .id_salt("lo_entries_scroll")
                        .max_height(260.0)
                        .show(&mut cols[0], |ui| {
                            for (i, e) in self.launch_options_modal.entries.iter().enumerate() {
                                let lbl = if !e.description.is_empty() {
                                    &e.description
                                } else if !e.executable.is_empty() {
                                    &e.executable
                                } else {
                                    "Entry"
                                };
                                let selected = self.launch_options_modal.selected_idx == Some(i);
                                if ui
                                    .selectable_label(selected, format!("[{}]  {}", i, lbl))
                                    .clicked()
                                {
                                    self.launch_options_modal.selected_idx = Some(i);
                                }
                            }
                        });
                    cols[0].horizontal(|ui| {
                        if secondary_button(ui, "+ ADD ENTRY").clicked() {
                            let idx = self.launch_options_modal.entries.len();
                            self.launch_options_modal.entries.push(LaunchEntry {
                                index: idx.to_string(),
                                launch_type: "default".into(),
                                ..Default::default()
                            });
                            self.launch_options_modal.selected_idx = Some(idx);
                        }
                        if danger_button(ui, "REMOVE").clicked() {
                            if let Some(sel) = self.launch_options_modal.selected_idx {
                                if sel < self.launch_options_modal.entries.len() {
                                    self.launch_options_modal.entries.remove(sel);
                                    self.launch_options_modal.selected_idx =
                                        (!self.launch_options_modal.entries.is_empty())
                                            .then_some(0);
                                }
                            }
                        }
                    });

                    if let Some(sel) = self.launch_options_modal.selected_idx {
                        if let Some(entry) = self.launch_options_modal.entries.get_mut(sel) {
                            egui::Grid::new("lo_edit_grid")
                                .num_columns(2)
                                .spacing([10.0, 8.0])
                                .show(&mut cols[1], |ui| {
                                    ui.label("Executable:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.executable)
                                            .code_editor()
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();

                                    ui.label("Arguments:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.arguments)
                                            .code_editor()
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();

                                    ui.label("Working Dir:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.workingdir)
                                            .code_editor()
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();

                                    ui.label("Description:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.description)
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();

                                    ui.label("Launch Type:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.launch_type)
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();

                                    ui.label("OS Filter:");
                                    ui.add(
                                        TextEdit::singleline(&mut entry.oslist)
                                            .hint_text("e.g. linux, windows")
                                            .desired_width(240.0),
                                    );
                                    ui.end_row();
                                });
                        }
                    } else {
                        cols[1].label(
                            RichText::new("No launch entries found or selected.")
                                .color(Palette::TEXT_MUTED),
                        );
                    }
                });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ghost_button(ui, "RESTORE APPINFO.VDF BACKUP").clicked() {
                        do_restore = true;
                    }
                    if !self.launch_options_modal.feedback.is_empty() {
                        ui.label(
                            RichText::new(&self.launch_options_modal.feedback)
                                .monospace()
                                .size(11.0)
                                .color(Palette::BLUE_LIGHT),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ghost_button(ui, "CANCEL").clicked() {
                            close = true;
                        }
                        if primary_button(ui, "SAVE TO APPINFO.VDF").clicked() {
                            do_save = true;
                        }
                    });
                });
            });

        if do_save {
            if let Some(inst) = &self.steam_service.installation {
                let svc = LaunchOptionsService::new(inst.root.clone());
                match svc.save_launch_entries(app_id, &self.launch_options_modal.entries) {
                    Ok(msg) => {
                        self.status_text = msg;
                        close = true;
                    }
                    Err(e) => self.launch_options_modal.feedback = e,
                }
            }
        }
        if do_restore {
            if let Some(inst) = &self.steam_service.installation {
                let svc = LaunchOptionsService::new(inst.root.clone());
                match svc.restore_backup() {
                    Ok(msg) => {
                        self.launch_options_modal.entries = svc.get_launch_entries(app_id);
                        self.launch_options_modal.selected_idx =
                            (!self.launch_options_modal.entries.is_empty()).then_some(0);
                        self.launch_options_modal.feedback = msg;
                    }
                    Err(e) => self.launch_options_modal.feedback = e,
                }
            }
        }
        if close {
            self.launch_options_modal.open = false;
        }
    }

    fn render_depot_detail_modal(&mut self, ctx: &egui::Context) {
        if !self.depot_detail_modal.open {
            return;
        }
        let mut close = false;
        let depot = self.depot_detail_modal.depot.clone();
        let entry = self.depot_detail_modal.entry.clone();
        let did = depot
            .as_ref()
            .map(|d| d.id)
            .or_else(|| entry.as_ref().map(|e| e.id))
            .unwrap_or(0);

        egui::Window::new(format!("DEPOT INSPECTOR // {}", did))
            .collapsible(false)
            .resizable(false)
            .fixed_size([480.0, 310.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("depot_detail_grid")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Depot ID:");
                        ui.label(RichText::new(did.to_string()).monospace().strong());
                        ui.end_row();

                        let sz = entry
                            .as_ref()
                            .and_then(|e| e.size_on_disk)
                            .or_else(|| depot.as_ref().map(|d| d.size))
                            .unwrap_or(0);
                        ui.label("Size:");
                        ui.label(RichText::new(format_bytes(sz)).monospace());
                        ui.end_row();

                        if let Some(d) = &depot {
                            let kind = if d.is_shared {
                                "Shared Redistributable"
                            } else if d.is_dlc() {
                                "DLC Content"
                            } else {
                                "Primary Game Content"
                            };
                            ui.label("Type:");
                            ui.label(kind);
                            ui.end_row();

                            if let Some(os) = &d.os {
                                ui.label("OS:");
                                ui.label(RichText::new(os).monospace());
                                ui.end_row();
                            }
                            if let Some(lang) = &d.language {
                                ui.label("Language:");
                                ui.label(RichText::new(lang).monospace());
                                ui.end_row();
                            }
                            if let Some(pm) = &d.public_manifest_id {
                                ui.label("Public Manifest:");
                                ui.label(RichText::new(pm).monospace());
                                ui.end_row();
                            }
                        }

                        if let Some(e) = &entry {
                            if let Some(pin) = &e.manifest_id {
                                ui.label("Active Pin:");
                                ui.label(
                                    RichText::new(pin)
                                        .monospace()
                                        .strong()
                                        .color(Palette::BLUE_LIGHT),
                                );
                                ui.end_row();
                            }
                            if let Some(cp) = &e.commented_manifest_id {
                                ui.label("Auto-Update GID:");
                                ui.label(RichText::new(cp).monospace());
                                ui.end_row();
                            }
                            if let Some(k) = &e.key {
                                let masked = if k.len() >= 16 {
                                    format!("{}…{}", &k[..10], &k[k.len() - 6..])
                                } else {
                                    k.clone()
                                };
                                ui.label("Decryption Key:");
                                ui.label(RichText::new(masked).monospace());
                                ui.end_row();
                            }
                        }
                    });

                ui.add_space(16.0);
                if secondary_button(ui, "CLOSE").clicked() {
                    close = true;
                }
            });

        if close {
            self.depot_detail_modal.open = false;
        }
    }

    fn render_text_prompt_modal(&mut self, ctx: &egui::Context) {
        if !self.text_prompt_modal.open {
            return;
        }
        let mut submit = false;
        let mut cancel = false;

        egui::Window::new(self.text_prompt_modal.title.clone())
            .collapsible(false)
            .resizable(false)
            .fixed_size([400.0, 140.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(&self.text_prompt_modal.prompt);
                let resp = ui.add(
                    TextEdit::singleline(&mut self.text_prompt_modal.value)
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "CONFIRM").clicked() {
                        submit = true;
                    }
                    if ghost_button(ui, "CANCEL").clicked() {
                        cancel = true;
                    }
                });
            });

        if submit {
            let val = self.text_prompt_modal.value.trim().to_string();
            match self.text_prompt_modal.kind.as_str() {
                "preset" => {
                    if let Some(aid) = self.depots_app_id {
                        let text = if self.depots_editing_live {
                            self.depots_editor_text.clone()
                        } else {
                            self.lua_vault.read_live_text(aid)
                        };
                        if !text.trim().is_empty() {
                            if let Some(v) = self.lua_vault.save_text(aid, &text, Some(&val)) {
                                self.status_text = format!("Saved preset: {}", v.display_label());
                                self.depots_variants = self.lua_vault.get_variants(aid);
                            }
                        }
                    }
                }
                "rename_variant" => {
                    if let Some(aid) = self.depots_app_id {
                        self.lua_vault
                            .rename(aid, &self.text_prompt_modal.target_hash, Some(&val));
                        self.depots_variants = self.lua_vault.get_variants(aid);
                    }
                }
                "bot_code" => {
                    if !val.is_empty() {
                        let auth = self.auth_service.clone();
                        let tx = self.msg_tx.clone();
                        let ctx_c = ctx.clone();
                        self.status_text = "Redeeming 6-char Discord bot code...".into();
                        thread::spawn(move || {
                            match auth.sign_in_with_code(&val) {
                                Ok(sess) => {
                                    let _ = tx.send(crate::ui::state::AppMessage::AuthCompleted {
                                        ok: true,
                                        msg: format!("Signed in as {}", sess.display_name),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(crate::ui::state::AppMessage::AuthCompleted {
                                        ok: false,
                                        msg: e,
                                    });
                                }
                            }
                            ctx_c.request_repaint();
                        });
                    }
                }
                _ => {}
            }
            self.text_prompt_modal.open = false;
        } else if cancel {
            self.text_prompt_modal.open = false;
        }
    }

    fn render_about_modal(&mut self, ctx: &egui::Context) {
        if !self.about_modal_open {
            return;
        }
        let mut close = false;
        egui::Window::new("ABOUT LUATOOLS // LINUX v2.0.0")
            .collapsible(false)
            .resizable(false)
            .fixed_size([500.0, 290.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("LUATOOLS // LINUX  v2.0.0 (NATIVE RUST EDITION)")
                        .monospace()
                        .strong()
                        .size(13.5)
                        .color(Palette::BLUE_LIGHT),
                );
                ui.add_space(6.0);
                ui.label(
                    "Lightweight, zero-Python native workstation for Steam stplug-in lua manifests, \
                     LuaVault variants, depot downloads, and SHA-256 backed game fixes.",
                );
                ui.add_space(8.0);
                for line in [
                    "• Pure-Rust hardware-accelerated GUI (eframe/egui) + rustls networking",
                    "• Steam Store search, Featured browse & multi-provider manifest FastFetch",
                    "• Global drag & drop (.lua / .manifest / .zip / URL) + Ctrl+K Quick Command",
                    "• Content-addressed LuaVault & live syntax-highlighted Lua workbench",
                    "• Binary Steam appcache/appinfo.vdf v27–v29 Launch Options editor",
                    "• SLSsteam config.yaml, Millennium RPC (:38495) & CEF CDP (:8080 -> :6767)",
                ] {
                    ui.label(RichText::new(line).size(12.0).color(Palette::TEXT_SECONDARY));
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "CLOSE").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.about_modal_open = false;
        }
    }
}
