//! Modal dialogs (`AddByAppIdModal`, `GameDetailModal`, `LaunchOptionsModal`,
//! `DepotDetailModal`, `TextPromptModal`, and `AboutModal`) styled in Papercut Monotone-Dark.

use std::thread;

use eframe::egui::{self, Align, ComboBox, Layout, Margin, RichText, ScrollArea, TextEdit};

use crate::parsers::vdf_binary::{LaunchEntry, LaunchOptionsService};
use crate::services::game_process_injection_service::InjectionConfig;
use crate::services::telemetry_service::TelemetryService;
use crate::ui::app::LuaToolsApp;
use crate::ui::lua_editor::validate_lua_text;
use crate::ui::state::{ActiveView, AppMessage, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    danger_button, display_font, draw_rem_medallion, format_bytes, ghost_button, inset_paper_sheet,
    lua_layouter, pick_files_native, primary_button, secondary_button, semibold_font, status_badge,
    BadgeTone, Palette,
};
use crate::VERSION;

impl LuaToolsApp {
    pub fn render_modals(&mut self, ctx: &egui::Context) {
        let any_modal_open = !self.settings_service.settings.telemetry_consent_seen
            || self.telemetry_modal_open
            || self.add_appid_modal.open
            || self.game_detail_modal.open
            || self.integration_modal.open
            || self.launch_options_modal.open
            || self.depot_detail_modal.open
            || self.text_prompt_modal.open
            || self.about_modal_open
            || self.guide_zoom_screenshot.is_some();
        if any_modal_open {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("modal_backdrop_scrim"),
            ));
            painter.rect_filled(
                screen,
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgba_unmultiplied(6, 8, 12, 185),
            );
        }

        let esc_pressed = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc_pressed && any_modal_open {
            if self.guide_zoom_screenshot.is_some() {
                self.guide_zoom_screenshot = None;
            } else if self.text_prompt_modal.open {
                self.text_prompt_modal.open = false;
            } else if self.depot_detail_modal.open {
                self.depot_detail_modal.open = false;
            } else if self.about_modal_open {
                self.about_modal_open = false;
            } else if self.add_appid_modal.open {
                self.add_appid_modal.open = false;
            } else if self.game_detail_modal.open {
                self.game_detail_modal.open = false;
            } else if self.launch_options_modal.open {
                self.launch_options_modal.open = false;
            } else if self.integration_modal.open {
                self.integration_modal.open = false;
            } else if self.telemetry_modal_open
                && self.settings_service.settings.telemetry_consent_seen
            {
                self.telemetry_modal_open = false;
            }
        }

        self.render_telemetry_consent_modal(ctx);
        self.render_add_by_appid_modal(ctx);
        self.render_game_detail_modal(ctx);
        self.render_integration_modal(ctx);
        self.render_launch_options_modal(ctx);
        self.render_depot_detail_modal(ctx);
        self.render_text_prompt_modal(ctx);
        self.render_about_modal(ctx);
        self.render_guide_zoom_modal(ctx);
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
            .order(egui::Order::Foreground)
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
            .order(egui::Order::Foreground)
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
                        let aid_resp = ui.add_sized(
                            [310.0, 30.0],
                            TextEdit::singleline(&mut self.add_appid_modal.app_id_input)
                                .hint_text("e.g. 1245620"),
                        );
                        if aid_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            create = true;
                        }
                        ui.end_row();

                        ui.label("Title:");
                        let title_resp = ui.add_sized(
                            [310.0, 30.0],
                            TextEdit::singleline(&mut self.add_appid_modal.title_input)
                                .hint_text("Optional game title comment"),
                        );
                        if title_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            create = true;
                        }
                        ui.end_row();

                        ui.label("Template:");
                        let templates = [
                            "Base App Only (addappid)",
                            "Base App + DLC Placeholder",
                            "Base App + Keyed Depot + Pinned Manifest",
                        ];
                        ComboBox::from_id_salt("tpl_combo")
                            .width(310.0)
                            .selected_text(
                                templates
                                    .get(self.add_appid_modal.template_idx)
                                    .copied()
                                    .unwrap_or(templates[0]),
                            )
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
            match self.add_appid_modal.app_id_input.trim().parse::<u32>() {
                Ok(aid) if aid > 0 => {
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
                            aid.saturating_add(10)
                        ),
                        _ => format!(
                            "addappid({}){}\naddappid({}, 1, \"{}\")\nsetManifestid({}, \"0000000000000000000\", 0)\n",
                            aid,
                            comment,
                            aid.saturating_add(1),
                            "0".repeat(64),
                            aid.saturating_add(1)
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
                            self.add_appid_modal.app_id_input.clear();
                            self.add_appid_modal.title_input.clear();
                            close = true;
                        } else {
                            self.notify(
                                res.error
                                    .unwrap_or_else(|| format!("Failed to create {}.lua", aid)),
                                ToastKind::Warning,
                            );
                        }
                    } else {
                        self.notify("Steam installation not detected.", ToastKind::Warning);
                    }
                }
                _ => {
                    self.notify(
                        "Enter a valid numeric AppID greater than 0.",
                        ToastKind::Warning,
                    );
                }
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
        .order(egui::Order::Foreground)
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
                        if !self.game_detail_modal.fake_id_input.trim().is_empty()
                            && ghost_button(ui, "Clear").clicked()
                        {
                            self.game_detail_modal.fake_id_input.clear();
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
            let report = validate_lua_text(&self.game_detail_modal.lua_text, app_id);
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.label(
                        RichText::new("stplug-in .lua Content")
                            .font(semibold_font(12.5))
                            .color(Palette::TEXT_SECONDARY),
                    );
                },
                |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} entries · {} depots · {} active pins",
                            report.parsed.entries.len(),
                            report.parsed.depot_count(),
                            report.parsed.active_pins.len()
                        ))
                        .size(11.5)
                        .color(Palette::TEXT_MUTED),
                    );
                },
            );
            if let Some(ref err) = report.error_message {
                ui.label(
                    RichText::new(format!("Syntax error: {}", err))
                        .size(11.5)
                        .color(Palette::DANGER_TEXT),
                );
            }
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
            let parsed_fid = if fid_trim.is_empty() {
                Ok(None)
            } else {
                match fid_trim.parse::<u32>() {
                    Ok(fid) if fid > 0 => Ok(Some(fid)),
                    _ => Err("FakeAppId must be a valid positive number (e.g. 480)."),
                }
            };

            match parsed_fid {
                Err(msg) => {
                    self.game_detail_modal.feedback = msg.into();
                }
                Ok(fid_opt) => {
                    match fid_opt {
                        None => {
                            let _ = self.slssteam_service.remove_fake_app_id(app_id);
                        }
                        Some(fid) => {
                            self.slssteam_service.set_fake_app_id(app_id, fid);
                        }
                    }

                    let tok_trim = self.game_detail_modal.token_input.trim().to_string();
                    if tok_trim.is_empty() {
                        self.slssteam_service.remove_token(app_id);
                    } else {
                        self.slssteam_service.set_token(app_id, tok_trim);
                    }
                    let sls_ok = self.slssteam_service.save();

                    let mut lua_err: Option<String> = None;
                    if !self.game_detail_modal.lua_text.trim().is_empty() {
                        if let Some(installer) = self.lua_installer.clone() {
                            let res = installer.install_lua(
                                app_id,
                                &self.game_detail_modal.lua_text,
                                true,
                                None,
                                Some("config-dialog"),
                                Some(&mut self.settings_service),
                                Some(&mut self.slssteam_service),
                            );
                            if !res.ok {
                                lua_err = res
                                    .error
                                    .or_else(|| Some(format!("Failed to write {}.lua", app_id)));
                            }
                        } else {
                            lua_err = Some(
                                "Steam installation not detected — .lua file not written.".into(),
                            );
                        }
                    }

                    self.refresh_all_local_state();
                    if let Some(err) = lua_err {
                        self.game_detail_modal.feedback = err.clone();
                        self.notify(err, ToastKind::Warning);
                    } else if !sls_ok {
                        self.game_detail_modal.feedback =
                            "Saved .lua, but failed to write SLSsteam config.yaml.".into();
                        self.notify(self.game_detail_modal.feedback.clone(), ToastKind::Warning);
                    } else {
                        self.notify(
                            format!("Saved overrides & lua for AppID {}", app_id),
                            ToastKind::Success,
                        );
                        close = true;
                    }
                }
            }
        }
        if do_delete {
            if let Some(installer) = self.lua_installer.clone() {
                let res = installer.uninstall_lua(app_id, true, Some(&mut self.slssteam_service));
                self.refresh_all_local_state();
                if res.ok {
                    self.notify(format!("Removed {}.lua", app_id), ToastKind::Info);
                    close = true;
                } else {
                    let err = res
                        .error
                        .unwrap_or_else(|| format!("Failed to remove {}.lua", app_id));
                    self.game_detail_modal.feedback = err.clone();
                    self.notify(err, ToastKind::Warning);
                }
            } else {
                self.game_detail_modal.feedback = "Steam installation not detected.".into();
                self.notify("Steam installation not detected.", ToastKind::Warning);
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
        egui::Window::new(format!(
            "Launch Options — {} (#{})",
            self.launch_options_modal.game_name, app_id
        ))
        .order(egui::Order::Foreground)
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
                            for (i, e) in self.launch_options_modal.entries.iter_mut().enumerate() {
                                e.index = i.to_string();
                            }
                            let len = self.launch_options_modal.entries.len();
                            self.launch_options_modal.selected_idx =
                                (len > 0).then(|| sel.min(len - 1));
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
                    Ok(m) => {
                        self.launch_options_modal.feedback = m.clone();
                        self.notify(m, ToastKind::Success);
                    }
                    Err(m) => {
                        self.launch_options_modal.feedback = m.clone();
                        self.notify(m, ToastKind::Warning);
                    }
                }
            } else {
                self.launch_options_modal.feedback = "Steam installation not detected.".into();
                self.notify("Steam installation not detected.", ToastKind::Warning);
            }
        }
        if do_restore {
            if let Some(inst) = &self.steam_service.installation {
                let svc = LaunchOptionsService::new(inst.root.clone());
                match svc.restore_backup() {
                    Ok(m) => {
                        let reloaded = svc.get_launch_entries(app_id);
                        self.launch_options_modal.selected_idx =
                            (!reloaded.is_empty()).then_some(0);
                        self.launch_options_modal.entries = reloaded;
                        self.launch_options_modal.feedback = m.clone();
                        self.notify(m, ToastKind::Success);
                    }
                    Err(m) => {
                        self.launch_options_modal.feedback = m.clone();
                        self.notify(m, ToastKind::Warning);
                    }
                }
            } else {
                self.launch_options_modal.feedback = "Steam installation not detected.".into();
                self.notify("Steam installation not detected.", ToastKind::Warning);
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
        let mut copy_item: Option<(&'static str, String)> = None;

        egui::Window::new("Depot Declaration Details")
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .fixed_size([490.0, 265.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("depot_detail_grid")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        if let Some(e) = &self.depot_detail_modal.entry {
                            ui.label("Depot / App ID:");
                            ui.horizontal(|ui| {
                                ui.monospace(e.id.to_string());
                                if ghost_button(ui, "Copy").clicked() {
                                    copy_item = Some(("Depot ID", e.id.to_string()));
                                }
                            });
                            ui.end_row();

                            ui.label("Decryption Key:");
                            ui.horizontal(|ui| match &e.key {
                                Some(k) => {
                                    ui.monospace(format_key_preview(k));
                                    if ghost_button(ui, "Copy").clicked() {
                                        copy_item = Some(("Decryption key", k.clone()));
                                    }
                                }
                                None => {
                                    ui.monospace("None");
                                }
                            });
                            ui.end_row();

                            ui.label("Pinned Manifest:");
                            let mf = e
                                .manifest_id
                                .as_deref()
                                .or(e.commented_manifest_id.as_deref());
                            ui.horizontal(|ui| {
                                ui.monospace(mf.unwrap_or("None"));
                                if let Some(gid) = mf {
                                    if ghost_button(ui, "Copy").clicked() {
                                        copy_item = Some(("Manifest GID", gid.to_string()));
                                    }
                                }
                            });
                            ui.end_row();
                        }
                        let size_bytes = self
                            .depot_detail_modal
                            .depot
                            .as_ref()
                            .map(|d| d.size)
                            .filter(|&s| s > 0)
                            .or_else(|| {
                                self.depot_detail_modal
                                    .entry
                                    .as_ref()
                                    .and_then(|e| e.size_on_disk)
                            });
                        if let Some(sz) = size_bytes {
                            ui.label("Size on Disk:");
                            ui.monospace(format_bytes(sz));
                            ui.end_row();
                        }
                        if let Some(d) = &self.depot_detail_modal.depot {
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

        if let Some((label, val)) = copy_item {
            ctx.copy_text(val);
            self.notify(format!("Copied {} to clipboard.", label), ToastKind::Info);
        }
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
            .order(egui::Order::Foreground)
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
            let mut close_prompt = true;
            match self.text_prompt_modal.kind.as_str() {
                "preset" => {
                    if let Some(aid) = self.depots_app_id {
                        let text = if self.depots_editing_live {
                            self.depots_editor_text.clone()
                        } else {
                            self.lua_vault.read_live_text(aid)
                        };
                        if text.trim().is_empty() {
                            self.notify(
                                "No .lua content available to save as a preset.",
                                ToastKind::Warning,
                            );
                        } else if let Some(v) = self.lua_vault.save_text(
                            aid,
                            &text,
                            (!val.is_empty()).then_some(val.as_str()),
                        ) {
                            self.notify(
                                format!("Saved preset: {}", v.display_label()),
                                ToastKind::Success,
                            );
                            self.depots_variants = self.lua_vault.get_variants(aid);
                        } else {
                            self.notify("Failed to save LuaVault preset.", ToastKind::Warning);
                        }
                    }
                }
                "rename_variant" => {
                    if let Some(aid) = self.depots_app_id {
                        self.lua_vault.rename(
                            aid,
                            &self.text_prompt_modal.target_hash,
                            (!val.is_empty()).then_some(val.as_str()),
                        );
                        self.depots_variants = self.lua_vault.get_variants(aid);
                        self.notify("Updated LuaVault variant label.", ToastKind::Info);
                    }
                }
                "bot_code" => {
                    if val.is_empty() {
                        self.notify(
                            "Enter the 6-character code from Discord /login.",
                            ToastKind::Warning,
                        );
                        close_prompt = false;
                    } else {
                        let auth = self.auth_service.clone();
                        let tx = self.msg_tx.clone();
                        let ctx_c = ctx.clone();
                        self.notify("Redeeming 6-char Discord bot code…", ToastKind::Info);
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
            if close_prompt {
                self.text_prompt_modal.open = false;
            }
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
            .order(egui::Order::Foreground)
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

    pub fn open_integration_modal(
        &mut self,
        app_id: u32,
        game_name: String,
        install_path: Option<std::path::PathBuf>,
    ) {
        let spoof = self
            .steam_bridge_service
            .get_app_id_spoof(app_id)
            .or_else(|| self.slssteam_service.get_fake_app_id(app_id))
            .unwrap_or(480)
            .to_string();
        let cd_key = self
            .steam_bridge_service
            .get_legacy_cd_key(app_id)
            .unwrap_or_default();
        let existing_inj = self.game_injection_service.get_config(app_id);
        let proc_enabled = existing_inj.is_some();
        let proc_lib = existing_inj
            .as_ref()
            .map(|c| c.library_path.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                self.game_injection_service
                    .default_library()
                    .to_string_lossy()
                    .to_string()
            });
        let proc_args = existing_inj
            .as_ref()
            .map(|c| c.launch_args.join(" "))
            .unwrap_or_default();

        self.integration_modal.open = true;
        self.integration_modal.app_id = app_id;
        self.integration_modal.game_name = game_name;
        self.integration_modal.install_path = install_path;
        self.integration_modal.spoof_id_input = spoof;
        self.integration_modal.cd_key_input = cd_key;
        self.integration_modal.depot_id_input = app_id.saturating_add(1).to_string();
        self.integration_modal.depot_key_input.clear();
        self.integration_modal.stat_name_input.clear();
        self.integration_modal.stat_value_input.clear();
        self.integration_modal.proc_enabled = proc_enabled;
        self.integration_modal.proc_lib_input = proc_lib;
        self.integration_modal.proc_args_input = proc_args;
        self.integration_modal.feedback.clear();
    }

    fn render_integration_modal(&mut self, ctx: &egui::Context) {
        if !self.integration_modal.open {
            return;
        }
        let app_id = self.integration_modal.app_id;
        let game_name = self.integration_modal.game_name.clone();
        let install_path = self.integration_modal.install_path.clone();

        let is_redirect = self.steam_bridge_service.is_redirect_app(app_id)
            || self
                .play_unowned_service
                .is_app_enabled(app_id, &self.slssteam_service);
        let cur_spoof = self
            .steam_bridge_service
            .get_app_id_spoof(app_id)
            .or_else(|| self.slssteam_service.get_fake_app_id(app_id));
        let cur_cdkey = self.steam_bridge_service.get_legacy_cd_key(app_id);
        let cur_stats = self.steam_bridge_service.get_stats(app_id);

        let mut close = false;
        let mut apply_quick_setup = false;
        let mut toggle_redirect: Option<bool> = None;
        let mut set_spoof = false;
        let mut rem_spoof = false;
        let mut set_cdkey = false;
        let mut rem_cdkey = false;
        let mut set_depot_key = false;
        let mut rem_depot_key = false;
        let mut set_stat = false;
        let mut rem_stat: Option<String> = None;
        let mut save_proc = false;
        let mut launch_proc = false;

        egui::Window::new(format!(
            "Integration & Process Controls — {} (#{})",
            game_name, app_id
        ))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .default_size([650.0, 560.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ScrollArea::vertical()
                .id_salt("integration_modal_scroll")
                .max_height(485.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;

                    // 0. One-Click Quick Setup (Recommended)
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            status_badge(ui, "RECOMMENDED", BadgeTone::Blue);
                            ui.label(
                                RichText::new("Quick Setup: Apply Recommended Fixes")
                                    .font(semibold_font(13.5))
                                    .color(Palette::TEXT_PRIMARY),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Automatically applies all recommended settings:")
                                .size(12.0)
                                .color(Palette::TEXT_SECONDARY),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("- Add to Play-Unowned (Steam Inlibrary Fix)")
                                .size(11.5)
                                .color(Palette::TEXT_MUTED),
                        );
                        ui.label(
                            RichText::new("- Set AppID redirect to 480 (online compatibility)")
                                .size(11.5)
                                .color(Palette::TEXT_MUTED),
                        );
                        ui.label(
                            RichText::new(
                                "- Apply native SteamAutoPatch (Goldberg) when game is installed",
                            )
                            .size(11.5)
                            .color(Palette::TEXT_MUTED),
                        );
                        ui.add_space(8.0);
                        if primary_button(ui, "Apply Recommended Fixes Now").clicked() {
                            apply_quick_setup = true;
                        }
                    });
                    ui.add_space(8.0);

                    // 1. Play-Unowned Redirect Set & AppID Spoof (480)
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(
                                    RichText::new(
                                        "1. Ownership & AppID Redirect (Bridge + SLSsteam)",
                                    )
                                    .font(semibold_font(13.5))
                                    .color(Palette::TEXT_PRIMARY),
                                );
                            },
                            |ui| {
                                if is_redirect {
                                    status_badge(ui, "In Redirect Set", BadgeTone::Ready);
                                } else {
                                    status_badge(ui, "Standard", BadgeTone::Muted);
                                }
                            },
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if !is_redirect {
                                if primary_button(ui, "Add to Redirect Set (Play Unowned)")
                                    .clicked()
                                {
                                    toggle_redirect = Some(true);
                                }
                            } else if danger_button(ui, "Remove from Redirect Set").clicked() {
                                toggle_redirect = Some(false);
                            }
                        });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("AppID Redirect (Spoof):");
                            ui.add_sized(
                                [110.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.spoof_id_input)
                                    .hint_text("480"),
                            );
                            if secondary_button(ui, "Set Redirect").clicked() {
                                set_spoof = true;
                            }
                            if cur_spoof.is_some() && ghost_button(ui, "Remove Redirect").clicked()
                            {
                                rem_spoof = true;
                            }
                        });
                    });

                    // 2. Legacy CD Key & Depot Key
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("2. Legacy CD Key & Depot Key")
                                .font(semibold_font(13.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.horizontal(|ui| {
                            ui.label("CD Key:");
                            ui.add_sized(
                                [260.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.cd_key_input)
                                    .hint_text("XXXX-YYYY-ZZZZ-WWWW"),
                            );
                            if secondary_button(ui, "Set Key").clicked() {
                                set_cdkey = true;
                            }
                            if cur_cdkey.is_some() && ghost_button(ui, "Remove").clicked() {
                                rem_cdkey = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Depot ID:");
                            ui.add_sized(
                                [95.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.depot_id_input)
                                    .hint_text("Depot"),
                            );
                            ui.label("Hex Key:");
                            ui.add_sized(
                                [225.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.depot_key_input)
                                    .hint_text("64-char hex AES-256 key"),
                            );
                            if secondary_button(ui, "Set").clicked() {
                                set_depot_key = true;
                            }
                            if ghost_button(ui, "Remove").clicked() {
                                rem_depot_key = true;
                            }
                        });
                    });

                    // 3. Steam Stat Overrides
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("3. Steam Stat / Achievement Overrides")
                                .font(semibold_font(13.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Stat:");
                            ui.add_sized(
                                [165.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.stat_name_input)
                                    .hint_text("stat_name"),
                            );
                            ui.label("Value:");
                            ui.add_sized(
                                [165.0, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.stat_value_input)
                                    .hint_text("value"),
                            );
                            if secondary_button(ui, "Set Stat").clicked() {
                                set_stat = true;
                            }
                        });
                        for (k, v) in &cur_stats {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("- {} = {}", k, v))
                                        .size(12.0)
                                        .color(Palette::TEXT_SECONDARY),
                                );
                                if ghost_button(ui, "Remove").clicked() {
                                    rem_stat = Some(k.clone());
                                }
                            });
                        }
                    });

                    // 4. Game Process LD_PRELOAD Integration
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("4. Game Process Integration (LD_PRELOAD)")
                                .font(semibold_font(13.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.checkbox(
                            &mut self.integration_modal.proc_enabled,
                            "Enable per-app LD_PRELOAD shared library integration",
                        );
                        ui.horizontal(|ui| {
                            ui.label("Library (.so):");
                            let w = (ui.available_width() - 105.0).max(160.0);
                            ui.add_sized(
                                [w, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.proc_lib_input)
                                    .hint_text("~/.local/lib/luatools/libsteam_integration.so"),
                            );
                            if secondary_button(ui, "Browse…").clicked() {
                                let tx = self.msg_tx.clone();
                                let ctx_c = ctx.clone();
                                thread::spawn(move || {
                                    let files = pick_files_native(
                                        "Select Shared Library (.so)",
                                        Some("*.so"),
                                    );
                                    if let Some(f) = files.into_iter().next() {
                                        let _ =
                                            tx.send(AppMessage::FilePickedForGameInjectionSo(f));
                                        ctx_c.request_repaint();
                                    }
                                });
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Launch Args:");
                            let w = (ui.available_width() - 16.0).max(160.0);
                            ui.add_sized(
                                [w, 28.0],
                                TextEdit::singleline(&mut self.integration_modal.proc_args_input)
                                    .hint_text("Optional extra CLI arguments…"),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if secondary_button(ui, "Save Process Config").clicked() {
                                save_proc = true;
                            }
                            if primary_button(ui, "Launch with Integration").clicked() {
                                save_proc = true;
                                launch_proc = true;
                            }
                        });
                    });
                });

            if !self.integration_modal.feedback.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&self.integration_modal.feedback)
                        .size(12.0)
                        .color(Palette::BLUE_LIGHT),
                );
            }

            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, "Done").clicked() {
                    close = true;
                }
            });
        });

        if let Some(enable) = toggle_redirect {
            if enable {
                self.steam_bridge_service.add_redirect_app(app_id);
                let _ = self
                    .play_unowned_service
                    .enable_for_app(app_id, &mut self.slssteam_service);
                self.integration_modal.feedback =
                    format!("Added AppID {} to play-unowned redirect set.", app_id);
            } else {
                self.steam_bridge_service.remove_redirect_app(app_id);
                let _ = self
                    .play_unowned_service
                    .disable_for_app(app_id, &mut self.slssteam_service);
                self.integration_modal.feedback =
                    format!("Removed AppID {} from redirect set.", app_id);
            }
            self.refresh_all_local_state();
        }
        if set_spoof {
            match self.integration_modal.spoof_id_input.trim().parse::<u32>() {
                Ok(sid) if sid > 0 => {
                    self.steam_bridge_service.set_app_id_spoof(app_id, sid);
                    self.slssteam_service.set_fake_app_id(app_id, sid);
                    let _ = self.slssteam_service.save();
                    self.refresh_all_local_state();
                    self.integration_modal.feedback =
                        format!("Set AppID redirect {} → {}.", app_id, sid);
                }
                _ => {
                    self.integration_modal.feedback =
                        "Enter a valid positive numeric spoof AppID.".into();
                }
            }
        }
        if rem_spoof {
            self.steam_bridge_service.remove_app_id_spoof(app_id);
            self.slssteam_service.remove_fake_app_id(app_id);
            let _ = self.slssteam_service.save();
            self.refresh_all_local_state();
            self.integration_modal.feedback = format!("Removed AppID redirect for {}.", app_id);
        }
        if set_cdkey {
            let k = self.integration_modal.cd_key_input.trim().to_string();
            if !k.is_empty() {
                self.steam_bridge_service.set_legacy_cd_key(app_id, k);
                self.integration_modal.feedback = "Saved legacy CD key.".into();
            } else {
                self.integration_modal.feedback = "Enter a CD key before saving.".into();
            }
        }
        if rem_cdkey {
            self.steam_bridge_service.remove_legacy_cd_key(app_id);
            self.integration_modal.cd_key_input.clear();
            self.integration_modal.feedback = "Removed legacy CD key.".into();
        }
        if set_depot_key {
            let did_opt = self
                .integration_modal
                .depot_id_input
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&d| d > 0);
            let hex = self.integration_modal.depot_key_input.trim().to_string();
            if let Some(did) = did_opt {
                if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    self.steam_bridge_service.set_depot_key(did, hex.clone());
                    self.slssteam_service.config.depot_keys.insert(did, hex);
                    let _ = self.slssteam_service.save();
                    self.integration_modal.depot_key_input.clear();
                    self.integration_modal.feedback =
                        format!("Stored decryption key for Depot {}.", did);
                } else {
                    self.integration_modal.feedback =
                        "Depot key must be 64 hexadecimal characters.".into();
                }
            } else {
                self.integration_modal.feedback = "Enter a valid positive numeric Depot ID.".into();
            }
        }
        if rem_depot_key {
            match self.integration_modal.depot_id_input.trim().parse::<u32>() {
                Ok(did) if did > 0 => {
                    self.steam_bridge_service.remove_depot_key(did);
                    self.slssteam_service.config.depot_keys.remove(&did);
                    let _ = self.slssteam_service.save();
                    self.integration_modal.feedback =
                        format!("Removed decryption key for Depot {}.", did);
                }
                _ => {
                    self.integration_modal.feedback =
                        "Enter a valid numeric Depot ID to remove.".into();
                }
            }
        }
        if set_stat {
            let sname = self.integration_modal.stat_name_input.trim().to_string();
            let sval = self.integration_modal.stat_value_input.trim().to_string();
            if !sname.is_empty() {
                self.steam_bridge_service
                    .set_stat(app_id, sname.clone(), sval);
                self.integration_modal.stat_name_input.clear();
                self.integration_modal.stat_value_input.clear();
                self.integration_modal.feedback = format!("Set stat '{}'.", sname);
            } else {
                self.integration_modal.feedback = "Enter a stat name before saving.".into();
            }
        }
        if let Some(sname) = rem_stat {
            self.steam_bridge_service.remove_stat(app_id, &sname);
            self.integration_modal.feedback = format!("Removed stat '{}'.", sname);
        }
        if launch_proc && !self.integration_modal.proc_lib_input.trim().is_empty() {
            self.integration_modal.proc_enabled = true;
        }
        if save_proc {
            let lib_trimmed = self.integration_modal.proc_lib_input.trim();
            if self.integration_modal.proc_enabled {
                if lib_trimmed.is_empty() {
                    self.integration_modal.feedback =
                        "Specify a shared library (.so) path before saving.".into();
                    launch_proc = false;
                } else {
                    let args: Vec<String> = self
                        .integration_modal
                        .proc_args_input
                        .split_whitespace()
                        .map(ToString::to_string)
                        .collect();
                    self.game_injection_service.configure(
                        app_id,
                        InjectionConfig {
                            app_id,
                            library_path: std::path::PathBuf::from(lib_trimmed),
                            env_vars: Default::default(),
                            launch_args: args,
                        },
                    );
                    self.integration_modal.feedback =
                        "Saved per-app LD_PRELOAD process integration config.".into();
                }
            } else {
                self.game_injection_service.unconfigure(app_id);
                self.integration_modal.feedback = "Disabled per-app process integration.".into();
            }
        }
        if launch_proc {
            match self
                .game_injection_service
                .launch_with_integration(app_id, install_path.as_deref())
            {
                Ok(m) => self.notify(m, ToastKind::Success),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
        if apply_quick_setup {
            let mut applied = Vec::<String>::new();
            let mut had_warning = false;

            // 1. Add to redirect set (play unowned)
            let is_redirect = self.steam_bridge_service.is_redirect_app(app_id)
                || self
                    .play_unowned_service
                    .is_app_enabled(app_id, &self.slssteam_service);
            if !is_redirect {
                self.steam_bridge_service.add_redirect_app(app_id);
                let _ = self
                    .play_unowned_service
                    .enable_for_app(app_id, &mut self.slssteam_service);
                applied.push("Added to Play-Unowned redirect set".to_string());
            }

            // 2. Set AppID redirect to 480
            let cur_spoof = self
                .steam_bridge_service
                .get_app_id_spoof(app_id)
                .or_else(|| self.slssteam_service.get_fake_app_id(app_id));
            if cur_spoof != Some(480) {
                self.steam_bridge_service.set_app_id_spoof(app_id, 480);
                self.slssteam_service.set_fake_app_id(app_id, 480);
                let _ = self.slssteam_service.save();
                applied.push("Set AppID redirect to 480".to_string());
            }
            self.integration_modal.spoof_id_input = "480".into();

            // 3. Run SteamAutoPatch if game is installed
            if let Some(path) = install_path {
                match self.steam_auto_crack.launch_for_app(app_id, &path) {
                    Ok(_) => applied.push("Launched SteamAutoPatch for game".to_string()),
                    Err(e) => {
                        had_warning = true;
                        applied.push(format!("SteamAutoPatch failed: {}", e));
                    }
                }
            } else {
                had_warning = true;
                applied
                    .push("Game install path not found - run SteamAutoPatch manually".to_string());
            }

            self.integration_modal.feedback = format!(
                "Applied recommended fixes for {}: {}",
                game_name,
                applied.join(", ")
            );
            self.notify(
                self.integration_modal.feedback.clone(),
                if had_warning {
                    ToastKind::Warning
                } else {
                    ToastKind::Success
                },
            );
            self.refresh_all_local_state();
        }
        if close {
            self.integration_modal.open = false;
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
