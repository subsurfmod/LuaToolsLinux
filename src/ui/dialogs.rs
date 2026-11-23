//! Modal dialogs (`AddByAppIdModal`, `GameDetailModal`, `LaunchOptionsModal`,
//! `DepotDetailModal`, `TextPromptModal`, and `AboutModal`) styled in Papercut Monotone-Dark.

use std::thread;

use eframe::egui::{self, Align, ComboBox, Layout, Margin, RichText, ScrollArea, TextEdit};

use crate::parsers::vdf_binary::{LaunchEntry, LaunchOptionsService};
use crate::services::telemetry_service::TelemetryService;
use crate::ui::app::LuaToolsApp;
use crate::ui::state::{ActiveView, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    danger_button, display_font, draw_rem_medallion, format_bytes, ghost_button, inset_paper_sheet,
    lua_layouter, primary_button, secondary_button, semibold_font, Palette,
};
use crate::VERSION;

impl LuaToolsApp {
    pub fn render_modals(&mut self, ctx: &egui::Context) {
        self.render_telemetry_consent_modal(ctx);
        self.render_add_by_appid_modal(ctx);
        self.render_game_detail_modal(ctx);
        self.render_launch_options_modal(ctx);
        self.render_depot_detail_modal(ctx);
        self.render_text_prompt_modal(ctx);
        self.render_about_modal(ctx);
    }

    fn render_telemetry_consent_modal(&mut self, ctx: &egui::Context) {
        let should_show =
            !self.settings_service.settings.telemetry_consent_seen || self.telemetry_modal_open;
        if !should_show {
            return;
        }

        let mut act_keep_enabled = false;
        let mut act_turn_off = false;
        let mut act_open_settings = false;

        egui::Window::new("Telemetry & Diagnostic Privacy Notice")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([560.0, 355.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    draw_rem_medallion(ui, 40.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(
                            RichText::new("Help Improve LuaTools Linux")
                                .font(display_font(19.0))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new(
                                "LuaTools can record lightweight, scrubbed diagnostic telemetry to help diagnose bugs and crashes.",
                            )
                            .size(12.5)
                            .color(Palette::TEXT_SECONDARY),
                        );
                    });
                });

                ui.add_space(10.0);
                inset_paper_sheet(ui, Margin::symmetric(14, 12), |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(
                        RichText::new("• Collected when enabled:")
                            .font(semibold_font(12.5))
                            .color(Palette::BLUE_LIGHT),
                    );
                    ui.label(
                        RichText::new(
                            "  App version, Linux distro & display server (Wayland/X11), scrubbed error codes (DepotDownloader, manifest, fix, or update failures), and panic file:line locations.",
                        )
                        .size(12.0)
                        .color(Palette::TEXT_SECONDARY),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("• Strictly never collected:")
                            .font(semibold_font(12.5))
                            .color(Palette::GO_TEXT),
                    );
                    ui.label(
                        RichText::new(
                            "  Steam usernames/passwords, Discord tokens, Hubcap API keys, 64-char depot decryption keys, emails, or personal home directory usernames (automatically scrubbed to '~').",
                        )
                        .size(12.0)
                        .color(Palette::TEXT_SECONDARY),
                    );
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "You can inspect every recorded event, clear stored logs, or turn telemetry off at any time in Settings › Telemetry & Logs.",
                    )
                    .size(12.0)
                    .color(Palette::TEXT_MUTED),
                );

                ui.add_space(14.0);
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        if danger_button(ui, "Turn Off Telemetry").clicked() {
                            act_turn_off = true;
                        }
                    },
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if primary_button(ui, "Keep Enabled").clicked() {
                            act_keep_enabled = true;
                        }
                        if ghost_button(ui, "Telemetry Settings ›").clicked() {
                            act_open_settings = true;
                        }
                    },
                );
            });

        if act_turn_off {
            self.settings_service.settings.telemetry_enabled = false;
            self.settings_service.settings.telemetry_consent_seen = true;
            self.settings_service.settings.telemetry_install_id.clear();
            let _ = self.settings_service.save();
            self.telemetry_service.set_enabled(false);
            self.telemetry_service.clear_all();
            self.telemetry_modal_open = false;
            self.notify(
                "Telemetry turned off — no diagnostic events will be collected.",
                ToastKind::Info,
            );
        } else if act_keep_enabled {
            self.settings_service.settings.telemetry_enabled = true;
            self.settings_service.settings.telemetry_consent_seen = true;
            if self
                .settings_service
                .settings
                .telemetry_install_id
                .trim()
                .is_empty()
            {
                self.settings_service.settings.telemetry_install_id =
                    TelemetryService::generate_install_id();
            }
            let _ = self.settings_service.save();
            self.telemetry_service.set_enabled(true);
            self.telemetry_service.record(
                "info",
                "telemetry",
                "consent_accepted",
                "User confirmed diagnostic telemetry collection",
            );
            self.telemetry_modal_open = false;
            self.notify(
                "Diagnostic telemetry active — you can turn it off anytime in Settings › Telemetry & Logs.",
                ToastKind::Success,
            );
        } else if act_open_settings {
            self.settings_service.settings.telemetry_consent_seen = true;
            let _ = self.settings_service.save();
            self.telemetry_modal_open = false;
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Telemetry;
        }
    }

    fn render_add_by_appid_modal(&mut self, ctx: &egui::Context) {
        if !self.add_appid_modal.open {
            return;
        }
        let mut close = false;
        let mut create = false;

        egui::Window::new("Create stplug-in .lua by AppID")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([450.0, 235.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                egui::Grid::new("add_appid_grid")
                    .num_columns(2)
                    .spacing([14.0, 12.0])
                    .show(ui, |ui| {
                        ui.label("AppID:");
                        ui.add_sized(
                            [310.0, 30.0],
                            TextEdit::singleline(&mut self.add_appid_modal.app_id_input)
                                .hint_text("e.g. 1245620"),
                        );
                        ui.end_row();

                        ui.label("Title:");
                        ui.add_sized(
                            [310.0, 30.0],
                            TextEdit::singleline(&mut self.add_appid_modal.title_input)
                                .hint_text("Optional game title comment"),
                        );
                        ui.end_row();

                        ui.label("Template:");
                        let templates = [
                            "Base App Only (addappid)",
                            "Base App + DLC Placeholder",
                            "Base App + Keyed Depot + Pinned Manifest",
                        ];
                        ComboBox::from_id_salt("tpl_combo")
                            .width(310.0)
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

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Create .lua").clicked() {
                        create = true;
                    }
                    if ghost_button(ui, "Cancel").clicked() {
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
                        Some("manual-create"),
                        Some(&mut self.settings_service),
                        Some(&mut self.slssteam_service),
                    );
                    if res.ok {
                        self.refresh_all_local_state();
                        self.notify(format!("Created {}.lua", aid), ToastKind::Success);
                        close = true;
                    }
                } else {
                    self.notify("Steam installation not detected.", ToastKind::Warning);
                }
            } else {
                self.notify("Enter a valid numeric AppID.", ToastKind::Warning);
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
        let mut do_save = false;
        let mut do_delete = false;
        let app_id = self.game_detail_modal.app_id;

        egui::Window::new(format!(
            "Configure {} (#{})",
            self.game_detail_modal.game_name, app_id
        ))
        .collapsible(false)
        .resizable(true)
        .default_size([620.0, 470.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("game_detail_overrides_grid")
                .num_columns(2)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    ui.label("SLSsteam FakeAppId:");
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [140.0, 28.0],
                            TextEdit::singleline(&mut self.game_detail_modal.fake_id_input)
                                .hint_text("e.g. 480"),
                        );
                        if ghost_button(ui, "Set 480 (Spacewar)").clicked() {
                            self.game_detail_modal.fake_id_input = "480".into();
                        }
                    });
                    ui.end_row();

                    ui.label("App Token:");
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [280.0, 28.0],
                            TextEdit::singleline(&mut self.game_detail_modal.token_input)
                                .password(!self.game_detail_modal.show_token)
                                .hint_text("Optional AppToken"),
                        );
                        if ghost_button(
                            ui,
                            if self.game_detail_modal.show_token {
                                "Hide"
                            } else {
                                "Show"
                            },
                        )
                        .clicked()
                        {
                            self.game_detail_modal.show_token = !self.game_detail_modal.show_token;
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(10.0);
            ui.label(
                RichText::new("stplug-in .lua Content")
                    .font(semibold_font(12.5))
                    .color(Palette::TEXT_SECONDARY),
            );
            let mut layouter =
                |ui: &egui::Ui, string: &str, wrap_width: f32| lua_layouter(ui, string, wrap_width);
            inset_paper_sheet(ui, Margin::same(10), |ui| {
                ScrollArea::vertical()
                    .id_salt("modal_lua_scroll")
                    .max_height(210.0)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut self.game_detail_modal.lua_text)
                                .code_editor()
                                .desired_rows(10)
                                .desired_width(f32::INFINITY)
                                .frame(false)
                                .layouter(&mut layouter),
                        );
                    });
            });

            if !self.game_detail_modal.feedback.is_empty() {
                ui.label(
                    RichText::new(&self.game_detail_modal.feedback)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }

            ui.add_space(10.0);
            egui::Sides::new().show(
                ui,
                |ui| {
                    if danger_button(ui, "Remove .lua").clicked() {
                        do_delete = true;
                    }
                },
                |ui| {
                    if ghost_button(ui, "Cancel").clicked() {
                        close = true;
                    }
                    if primary_button(ui, "Save Configuration").clicked() {
                        do_save = true;
                    }
                },
            );
        });

        if do_save {
            let fid_trim = self.game_detail_modal.fake_id_input.trim();
            if fid_trim.is_empty() {
                self.slssteam_service.remove_fake_app_id(app_id);
            } else if let Ok(fid) = fid_trim.parse::<u32>() {
                self.slssteam_service.set_fake_app_id(app_id, fid);
            }

            let tok_trim = self.game_detail_modal.token_input.trim().to_string();
            if tok_trim.is_empty() {
                self.slssteam_service.remove_token(app_id);
            } else {
                self.slssteam_service.set_token(app_id, tok_trim);
            }
            let _ = self.slssteam_service.save();

            if !self.game_detail_modal.lua_text.trim().is_empty() {
                if let Some(installer) = self.lua_installer.clone() {
                    let _ = installer.install_lua(
                        app_id,
                        &self.game_detail_modal.lua_text,
                        true,
                        None,
                        Some("config-dialog"),
                        Some(&mut self.settings_service),
                        Some(&mut self.slssteam_service),
                    );
                }
            }
            self.refresh_all_local_state();
            self.notify(
                format!("Saved overrides & lua for AppID {}", app_id),
                ToastKind::Success,
            );
            close = true;
        }
        if do_delete {
            if let Some(installer) = self.lua_installer.clone() {
                let _ = installer.uninstall_lua(app_id, true, Some(&mut self.slssteam_service));
                self.refresh_all_local_state();
            }
            close = true;
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
        egui::Window::new(format!(
            "Launch Options — {} (#{})",
            self.launch_options_modal.game_name, app_id
        ))
        .collapsible(false)
        .resizable(true)
        .default_size([680.0, 460.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(Palette::WARN_BG)
                .stroke(egui::Stroke::new(1.0_f32, Palette::WARN_BORDER))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "Close Steam completely before saving changes to appinfo.vdf so Steam does not overwrite them on exit.",
                        )
                        .size(12.0)
                        .color(Palette::WARN_TEXT),
                    );
                });

            ui.add_space(10.0);
            ui.columns(2, |cols| {
                cols[0].label(RichText::new("Entries").font(semibold_font(12.5)));
                ScrollArea::vertical()
                    .id_salt("launch_entries_scroll")
                    .max_height(250.0)
                    .show(&mut cols[0], |ui| {
                        for (idx, e) in self.launch_options_modal.entries.iter().enumerate() {
                            let lbl = if e.description.is_empty() {
                                format!("[{}] {}", idx, e.executable)
                            } else {
                                format!("[{}] {} — {}", idx, e.description, e.executable)
                            };
                            if ui
                                .selectable_label(
                                    self.launch_options_modal.selected_idx == Some(idx),
                                    lbl,
                                )
                                .clicked()
                            {
                                self.launch_options_modal.selected_idx = Some(idx);
                            }
                        }
                    });
                cols[0].horizontal(|ui| {
                    if secondary_button(ui, "+ Add").clicked() {
                        let next = self.launch_options_modal.entries.len();
                        self.launch_options_modal.entries.push(LaunchEntry {
                            index: next.to_string(),
                            launch_type: "default".into(),
                            ..Default::default()
                        });
                        self.launch_options_modal.selected_idx = Some(next);
                    }
                    if let Some(sel) = self.launch_options_modal.selected_idx {
                        if sel < self.launch_options_modal.entries.len()
                            && danger_button(ui, "Remove").clicked()
                        {
                            self.launch_options_modal.entries.remove(sel);
                            self.launch_options_modal.selected_idx = None;
                        }
                    }
                });

                if let Some(sel) = self.launch_options_modal.selected_idx {
                    if let Some(entry) = self.launch_options_modal.entries.get_mut(sel) {
                        egui::Grid::new("launch_entry_edit_grid")
                            .num_columns(2)
                            .spacing([10.0, 8.0])
                            .show(&mut cols[1], |ui| {
                                ui.label("Executable:");
                                ui.add(TextEdit::singleline(&mut entry.executable));
                                ui.end_row();
                                ui.label("Arguments:");
                                ui.add(TextEdit::singleline(&mut entry.arguments));
                                ui.end_row();
                                ui.label("Working Dir:");
                                ui.add(TextEdit::singleline(&mut entry.workingdir));
                                ui.end_row();
                                ui.label("Description:");
                                ui.add(TextEdit::singleline(&mut entry.description));
                                ui.end_row();
                                ui.label("OS List:");
                                ui.add(
                                    TextEdit::singleline(&mut entry.oslist)
                                        .hint_text("windows / linux"),
                                );
                                ui.end_row();
                            });
                    }
                }
            });

            if !self.launch_options_modal.feedback.is_empty() {
                ui.label(
                    RichText::new(&self.launch_options_modal.feedback)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }

            ui.add_space(10.0);
            egui::Sides::new().show(
                ui,
                |ui| {
                    if secondary_button(ui, "Restore Backup").clicked() {
                        do_restore = true;
                    }
                },
                |ui| {
                    if ghost_button(ui, "Close").clicked() {
                        close = true;
                    }
                    if primary_button(ui, "Write to appinfo.vdf").clicked() {
                        do_save = true;
                    }
                },
            );
        });

        if do_save {
            if let Some(inst) = &self.steam_service.installation {
                let svc = LaunchOptionsService::new(inst.root.clone());
                match svc.save_launch_entries(app_id, &self.launch_options_modal.entries) {
                    Ok(m) | Err(m) => self.launch_options_modal.feedback = m,
                }
            }
        }
        if do_restore {
            if let Some(inst) = &self.steam_service.installation {
                let svc = LaunchOptionsService::new(inst.root.clone());
                match svc.restore_backup() {
                    Ok(m) | Err(m) => self.launch_options_modal.feedback = m,
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

        egui::Window::new("Depot Declaration Details")
            .collapsible(false)
            .resizable(false)
            .fixed_size([460.0, 250.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("depot_detail_grid")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        if let Some(e) = &self.depot_detail_modal.entry {
                            ui.label("Depot / App ID:");
                            ui.monospace(e.id.to_string());
                            ui.end_row();

                            ui.label("Decryption Key:");
                            ui.monospace(match &e.key {
                                Some(k) => format_key_preview(k),
                                None => "None".into(),
                            });
                            ui.end_row();

                            ui.label("Pinned Manifest:");
                            ui.monospace(
                                e.manifest_id
                                    .as_deref()
                                    .or(e.commented_manifest_id.as_deref())
                                    .unwrap_or("None"),
                            );
                            ui.end_row();
                        }
                        if let Some(d) = &self.depot_detail_modal.depot {
                            ui.label("Size on Disk:");
                            ui.monospace(format_bytes(d.size));
                            ui.end_row();
                            if let Some(os) = &d.os {
                                ui.label("Platform:");
                                ui.label(os);
                                ui.end_row();
                            }
                        }
                    });

                ui.add_space(16.0);
                if secondary_button(ui, "Close").clicked() {
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
            .fixed_size([400.0, 145.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(&self.text_prompt_modal.prompt);
                let resp = ui.add_sized(
                    [ui.available_width(), 30.0],
                    TextEdit::singleline(&mut self.text_prompt_modal.value),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Confirm").clicked() {
                        submit = true;
                    }
                    if ghost_button(ui, "Cancel").clicked() {
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
                                self.notify(
                                    format!("Saved preset: {}", v.display_label()),
                                    ToastKind::Success,
                                );
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
                "bot_code" if !val.is_empty() => {
                    let auth = self.auth_service.clone();
                    let tx = self.msg_tx.clone();
                    let ctx_c = ctx.clone();
                    self.status_text = "Redeeming 6-char Discord bot code…".into();
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
        egui::Window::new(format!("About LuaTools Linux v{}", VERSION))
            .collapsible(false)
            .resizable(false)
            .fixed_size([490.0, 275.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!("LuaTools Linux  v{}", VERSION))
                        .font(display_font(19.0))
                        .color(Palette::TEXT_PRIMARY),
                );
                ui.add_space(4.0);
                ui.label(
                    "Native Rust workstation for Steam stplug-in manifests, LuaVault variants, \
                     depot downloads, and SHA-256 verified game fixes.",
                );
                ui.add_space(10.0);
                for line in [
                    "• Tactile Papercut Monotone-Dark interface (eframe / egui + rustls)",
                    "• Steam Store search, featured browse & provider FastFetch",
                    "• Content-addressed LuaVault & live syntax-highlighted .lua editor",
                    "• Binary Steam appcache/appinfo.vdf v27–v29 Launch Options editor",
                    "• Background Git update check with one-click rebuild & restart",
                ] {
                    ui.label(
                        RichText::new(line)
                            .size(12.5)
                            .color(Palette::TEXT_SECONDARY),
                    );
                }
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "Close").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.about_modal_open = false;
        }
    }
}

fn format_key_preview(k: &str) -> String {
    let chars: Vec<char> = k.chars().collect();
    if chars.len() > 16 {
        let head: String = chars[..8].iter().collect();
        let tail: String = chars[chars.len() - 8..].iter().collect();
        format!("{}…{}", head, tail)
    } else {
        k.to_string()
    }
}
