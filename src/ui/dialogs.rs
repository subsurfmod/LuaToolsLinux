//! Modal dialogs (`AddByAppIdModal`, `GameDetailModal`, `LaunchOptionsModal`,
//! `DepotDetailModal`, `TextPromptModal`, and `AboutModal`) styled in Papercut Monotone-Dark.

use std::thread;

use eframe::egui::{self, Align, ComboBox, Layout, Margin, RichText, ScrollArea, TextEdit};

use crate::parsers::vdf_binary::LaunchEntry;
use crate::services::game_process_injection_service::InjectionConfig;
use crate::services::telemetry_service::TelemetryService;
use crate::services::update_service::UpdateService;
use crate::ui::app::{
    ActionModalState, LuaToolsApp, PendingConfirmAction, UiInternalMsg, UpdateOutcomeState,
};
use crate::ui::lua_editor::validate_lua_text;
use crate::ui::state::{ActiveView, AppMessage, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    danger_button, display_font, draw_rem_medallion, format_bytes, ghost_button, inset_paper_sheet,
    lua_layouter, pick_files_native, primary_button, secondary_button, semibold_font, status_badge,
    BadgeTone, Palette,
};
use crate::VERSION;

const PAPER_GRAIN_FALLBACK_BYTES: &[u8] = include_bytes!("../../assets/paper_grain_fallback.jpg");

impl LuaToolsApp {
    pub fn render_modals(&mut self, ctx: &egui::Context) {
        let show_first_run = (!self.settings_service.settings.telemetry_consent_seen
            && !self.started_hidden_or_silent)
            || self.telemetry_modal_open;
        let any_modal_open = show_first_run
            || self.emblem_overlay_open
            || self.confirm_modal.open
            || self.updater_feed.modal_open
            || self.action_modal.open
            || self.add_appid_modal.open
            || self.game_detail_modal.open
            || self.integration_modal.open
            || self.launch_options_modal.open
            || self.depot_detail_modal.open
            || self.text_prompt_modal.open
            || self.sudo_password_modal.open
            || self.about_modal_open
            || self.overwrite_diff_modal.open
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
            if self.emblem_overlay_open {
                self.close_emblem_overlay();
            } else if self.confirm_modal.open {
                self.confirm_modal.open = false;
                self.confirm_modal.action = None;
            } else if self.action_modal.open
                && self.action_modal.state != ActionModalState::Running
            {
                self.action_modal.open = false;
            } else if self.updater_feed.modal_open {
                self.updater_feed.modal_open = false;
            } else if self.guide_zoom_screenshot.is_some() {
                self.guide_zoom_screenshot = None;
            } else if self.sudo_password_modal.open {
                self.sudo_password_modal.clear_secret();
                self.sudo_password_modal.open = false;
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
                self.launch_options_rebased_app = None;
            } else if self.integration_modal.open {
                self.integration_modal.open = false;
            } else if self.overwrite_diff_modal.open {
                if self.overwrite_diff_modal.cleanup_parent {
                    if let Some(ref pkg) = self.overwrite_diff_modal.pending_package_path {
                        if let Some(dir) = pkg.parent() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                    }
                }
                self.overwrite_diff_modal.open = false;
                self.advance_overwrite_diff_queue();
            } else if self.telemetry_modal_open
                && self.settings_service.settings.telemetry_consent_seen
            {
                self.telemetry_modal_open = false;
            }
        }

        self.render_telemetry_consent_modal(ctx);
        self.render_overwrite_diff_modal(ctx);
        self.render_add_by_appid_modal(ctx);
        self.render_game_detail_modal(ctx);
        self.render_integration_modal(ctx);
        self.render_launch_options_modal(ctx);
        self.render_depot_detail_modal(ctx);
        self.render_text_prompt_modal(ctx);
        self.render_sudo_password_modal(ctx);
        self.render_about_modal(ctx);
        self.render_guide_zoom_modal(ctx);
        self.render_confirm_action_modal(ctx);
        self.render_update_progress_modal(ctx);
        self.render_action_progress_modal(ctx);
        self.render_emblem_overlay(ctx);
    }

    fn render_update_progress_modal(&mut self, ctx: &egui::Context) {
        if !self.updater_feed.modal_open {
            return;
        }
        let mut do_close = false;
        let mut do_retry = false;
        let mut do_relaunch: Option<std::path::PathBuf> = None;

        let is_running = self.updater_feed.is_running();
        let (badge_txt, badge_tone) = match &self.updater_feed.outcome {
            UpdateOutcomeState::Running => ("Compiling…", BadgeTone::Blue),
            UpdateOutcomeState::Succeeded { .. } => ("Ready to Restart", BadgeTone::Ready),
            UpdateOutcomeState::Failed { .. } => ("Build Failed", BadgeTone::Danger),
            UpdateOutcomeState::Idle => ("Idle", BadgeTone::Muted),
        };
        let exe_to_relaunch = match &self.updater_feed.outcome {
            UpdateOutcomeState::Succeeded { exe_path, .. } => Some(exe_path.clone()),
            _ => None,
        };
        let is_failed = matches!(self.updater_feed.outcome, UpdateOutcomeState::Failed { .. });

        egui::Window::new("LuaTools Self-Updater (Live Compiler Feed)")
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            .default_size([680.0, 415.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        ui.label(
                            RichText::new("Git Pull & Native Rust Release Build")
                                .font(display_font(17.5))
                                .color(Palette::TEXT_PRIMARY),
                        );
                    },
                    |ui| {
                        status_badge(ui, badge_txt, badge_tone);
                    },
                );
                ui.add_space(6.0);

                self.render_updater_progress_panel(ui, ctx, "updater_modal_feed_scroll", 220.0);

                ui.add_space(12.0);
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        if is_running {
                            ui.label(
                                RichText::new(
                                    "You can hide this window and re-open it anytime via the top-right % badge.",
                                )
                                .size(11.5)
                                .color(Palette::TEXT_MUTED),
                            );
                        }
                    },
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if is_running {
                            if ghost_button(ui, "Hide to Background").clicked() {
                                do_close = true;
                            }
                        } else {
                            if ghost_button(ui, "Close").clicked() {
                                do_close = true;
                            }
                            if let Some(ref exe) = exe_to_relaunch {
                                if primary_button(ui, "↻ Restart to Apply").clicked() {
                                    do_relaunch = Some(exe.clone());
                                }
                            } else if is_failed && primary_button(ui, "Retry Update").clicked() {
                                do_retry = true;
                            }
                        }
                    },
                );
            });

        if do_close {
            self.updater_feed.modal_open = false;
        }
        if do_retry {
            self.trigger_git_self_update(ctx);
        }
        if let Some(exe) = do_relaunch {
            self.notify("Relaunching updated LuaTools…", ToastKind::Info);
            match UpdateService::relaunch_binary(&exe) {
                Ok(()) => self.request_quit(ctx),
                Err(e) => self.notify(e, ToastKind::Warning),
            }
        }
    }

    fn render_action_progress_modal(&mut self, ctx: &egui::Context) {
        if !self.action_modal.open {
            return;
        }
        let state = self.action_modal.state;
        let is_running = state == ActionModalState::Running;
        if is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        let title = if self.action_modal.title.is_empty() {
            "Working…".to_string()
        } else {
            self.action_modal.title.clone()
        };
        let status = self.action_modal.status.clone();
        let elapsed = self.action_modal.elapsed_mm_ss();
        let progress_opt = self.action_modal.progress;
        let lines: Vec<String> = self.action_modal.detail_lines.iter().cloned().collect();
        let mut do_close = false;

        egui::Window::new(title)
            .id(egui::Id::new("unified_action_progress_modal"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .fixed_size([510.0, 255.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let status_col = match state {
                        ActionModalState::Succeeded => Palette::GO_TEXT,
                        ActionModalState::Failed => Palette::DANGER_TEXT,
                        _ => Palette::TEXT_PRIMARY,
                    };
                    let left_w = (ui.available_width() - 118.0).max(180.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(left_w, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&status)
                                        .font(semibold_font(13.0))
                                        .color(status_col),
                                )
                                .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        match state {
                            ActionModalState::Running => {
                                status_badge(ui, &format!("Running · {}", elapsed), BadgeTone::Blue);
                            }
                            ActionModalState::Succeeded => {
                                status_badge(ui, &format!("Done · {}", elapsed), BadgeTone::Ready);
                            }
                            ActionModalState::Failed => {
                                status_badge(ui, &format!("Failed · {}", elapsed), BadgeTone::Danger);
                            }
                            ActionModalState::Idle => {}
                        }
                    });
                });

                ui.add_space(8.0);
                if is_running {
                    if let Some(p) = progress_opt {
                        ui.add(
                            egui::ProgressBar::new(p.clamp(0.0, 1.0))
                                .show_percentage()
                                .animate(true)
                                .corner_radius(egui::CornerRadius::same(3)),
                        );
                    } else {
                        let t = ctx.input(|i| i.time) as f32;
                        let pulse = 0.25 + 0.55 * ((t * 1.4).sin() * 0.5 + 0.5);
                        ui.add(
                            egui::ProgressBar::new(pulse)
                                .text("Working…")
                                .animate(true)
                                .corner_radius(egui::CornerRadius::same(3)),
                        );
                    }
                } else if state == ActionModalState::Succeeded {
                    ui.add(
                        egui::ProgressBar::new(1.0)
                            .text("100% — Complete")
                            .corner_radius(egui::CornerRadius::same(3)),
                    );
                }

                ui.add_space(8.0);
                inset_paper_sheet(ui, Margin::same(8), |ui| {
                    ui.set_width(ui.available_width());
                    ScrollArea::vertical()
                        .id_salt("action_progress_modal_log_scroll")
                        .stick_to_bottom(true)
                        .max_height(92.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            for ln in &lines {
                                let col = if ln.starts_with("Failed:") {
                                    Palette::DANGER_TEXT
                                } else if ln.starts_with("Done:") {
                                    Palette::GO_TEXT
                                } else {
                                    Palette::TEXT_SECONDARY
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(ln)
                                            .font(egui::FontId::new(
                                                11.5,
                                                egui::FontFamily::Monospace,
                                            ))
                                            .color(col),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            }
                        });
                });

                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if is_running {
                        if ghost_button(ui, "Hide").clicked() {
                            do_close = true;
                        }
                    } else if primary_button(ui, "Close").clicked() {
                        do_close = true;
                    }
                });
            });

        if do_close {
            self.action_modal.open = false;
        }
    }

    fn render_confirm_action_modal(&mut self, ctx: &egui::Context) {
        if !self.confirm_modal.open {
            return;
        }
        let title = self.confirm_modal.title.clone();
        let message = self.confirm_modal.message.clone();
        let confirm_label = if self.confirm_modal.confirm_label.is_empty() {
            "Confirm".to_string()
        } else {
            self.confirm_modal.confirm_label.clone()
        };
        let is_danger = self.confirm_modal.is_danger;
        let mut do_confirm = false;
        let mut do_cancel = false;

        egui::Window::new(title)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .fixed_size([460.0, 175.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(message)
                        .size(13.0)
                        .color(Palette::TEXT_SECONDARY),
                );
                ui.add_space(16.0);
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        if ghost_button(ui, "Cancel").clicked() {
                            do_cancel = true;
                        }
                    },
                    |ui| {
                        let clicked = if is_danger {
                            danger_button(ui, &confirm_label).clicked()
                        } else {
                            primary_button(ui, &confirm_label).clicked()
                        };
                        if clicked {
                            do_confirm = true;
                        }
                    },
                );
            });

        if do_confirm {
            let action = self.confirm_modal.action.take();
            self.confirm_modal.open = false;
            if let Some(act) = action {
                match act {
                    PendingConfirmAction::KillSteam => {
                        self.trigger_kill_steam(ctx);
                    }
                    PendingConfirmAction::RestartSteam => {
                        self.trigger_restart_steam(ctx);
                    }
                    PendingConfirmAction::LaunchSteamWithBridge => {
                        self.launch_steam_with_bridge(ctx);
                    }
                    PendingConfirmAction::RevertFix {
                        app_id,
                        fix_id,
                        dir,
                    } => {
                        self.execute_revert_fix_async(app_id, fix_id, dir, ctx);
                    }
                    PendingConfirmAction::ReconcilePicsDrift { drifted_count } => {
                        self.start_action_modal(
                            "pics",
                            "Rebase Steam Launch Options",
                            format!(
                                "Reconciling Steam PICS update for {} modified app(s)…",
                                drifted_count
                            ),
                            None,
                        );
                        self.trigger_reconcile_pics_drift_async(ctx);
                    }
                }
            }
        } else if do_cancel {
            self.confirm_modal.open = false;
            self.confirm_modal.action = None;
        }
    }

    fn render_telemetry_consent_modal(&mut self, ctx: &egui::Context) {
        // LT-09: Skip First-Run Quick Setup automatically on --minimized / --silent launches
        let should_show = (!self.settings_service.settings.telemetry_consent_seen
            && !self.started_hidden_or_silent)
            || self.telemetry_modal_open;
        if !should_show {
            return;
        }

        let mut act_keep_enabled = false;
        let mut act_turn_off = false;
        let mut act_open_settings = false;
        let sls_installed = self.wired_service.is_slssteam_installed();
        let steam_running = crate::services::steam_service::SteamService::is_steam_running();

        egui::Window::new("First-Run Quick Setup & Privacy Notice")
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([580.0, 445.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    draw_rem_medallion(ui, 40.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(
                            RichText::new("Welcome to LuaTools Linux")
                                .font(display_font(19.0))
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.label(
                            RichText::new(
                                "Review diagnostic telemetry and choose your First-Run Quick Setup steps.",
                            )
                            .size(12.5)
                            .color(Palette::TEXT_SECONDARY),
                        );
                    });
                });

                ui.add_space(10.0);
                inset_paper_sheet(ui, Margin::symmetric(14, 10), |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 5.0;
                    ui.label(
                        RichText::new("• Diagnostic telemetry (when enabled):")
                            .font(semibold_font(12.5))
                            .color(Palette::BLUE_LIGHT),
                    );
                    ui.label(
                        RichText::new(
                            "  App version, Linux distro & display server (Wayland/X11), scrubbed error codes, and panic file:line locations.",
                        )
                        .size(11.5)
                        .color(Palette::TEXT_SECONDARY),
                    );
                    ui.label(
                        RichText::new("• Strictly never collected:")
                            .font(semibold_font(12.5))
                            .color(Palette::GO_TEXT),
                    );
                    ui.label(
                        RichText::new(
                            "  Steam credentials, Discord tokens, Hubcap API keys, 64-char depot decryption keys, emails, or home usernames.",
                        )
                        .size(11.5)
                        .color(Palette::TEXT_SECONDARY),
                    );
                });

                ui.add_space(8.0);
                inset_paper_sheet(ui, Margin::symmetric(14, 10), |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 5.0;
                    egui::Sides::new().show(
                        ui,
                        |ui| {
                            ui.checkbox(
                                &mut self.first_run_quick_setup,
                                "Run Guided First-Time Setup (URI links, Store Plugin & SLSsteam)",
                            );
                        },
                        |ui| {
                            if sls_installed {
                                status_badge(ui, "SLSsteam Ready", BadgeTone::Ready);
                            } else {
                                status_badge(ui, "SLSsteam Missing", BadgeTone::Warn);
                            }
                        },
                    );
                    ui.add_enabled_ui(self.first_run_quick_setup, |ui| {
                        ui.indent("quick_setup_steps", |ui| {
                            ui.checkbox(
                                &mut self.first_run_install_slssteam,
                                if sls_installed {
                                    "Verify Steam Unlocker & download full LTSP Steam Store Plugin"
                                } else {
                                    "Install Steam Unlocker & download full LTSP Steam Store Plugin"
                                },
                            );
                            ui.checkbox(
                                &mut self.first_run_enable_fast_fetch,
                                "Enable FastFetch (automatically pick fastest available manifest source)",
                            );
                            ui.checkbox(
                                &mut self.first_run_restart_steam,
                                if steam_running {
                                    "Restart Steam after setup so SLSsteam & Store Plugin take effect"
                                } else {
                                    "Launch Steam via SLSsteam after setup"
                                },
                            );
                        });
                    });
                });

                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "You can inspect every recorded event, clear stored logs, or change these options anytime in Settings.",
                    )
                    .size(11.5)
                    .color(Palette::TEXT_MUTED),
                );

                ui.add_space(12.0);
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        if danger_button(ui, "Turn Off Telemetry").clicked() {
                            act_turn_off = true;
                        }
                    },
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if primary_button(ui, "Keep Enabled & Continue").clicked() {
                            act_keep_enabled = true;
                        }
                        if ghost_button(ui, "Telemetry Settings ›").clicked() {
                            act_open_settings = true;
                        }
                    },
                );
            });

        if (act_turn_off || act_keep_enabled || act_open_settings) && self.first_run_quick_setup {
            let _ = crate::services::protocol_service::ProtocolService::register_desktop_handler();
            self.settings_service.settings.fast_fetch = self.first_run_enable_fast_fetch;
            let _ = self.slssteam_service.save();

            let steam_root_owned = self
                .steam_service
                .installation
                .as_ref()
                .map(|i| i.root.clone());
            if self.first_run_install_slssteam {
                if !sls_installed {
                    self.trigger_wired_action("steam_unlocker", ctx);
                }
                let ui_tx = self.ui_tx.clone();
                let ctx_c = ctx.clone();
                thread::spawn(move || {
                    let plugin_svc =
                        crate::services::plugin_installer_service::PluginInstallerService::new(
                            None,
                        );
                    let res = plugin_svc.download_and_install_ltsp(steam_root_owned.as_deref());
                    let (ok, msg) = match res {
                        Ok(p) => (
                            true,
                            format!("Quick Setup: Installed LTSP Store Plugin to {}", p.display()),
                        ),
                        Err(e) => {
                            let _ = plugin_svc
                                .ensure_luatools_js_installed(steam_root_owned.as_deref());
                            (
                                false,
                                format!(
                                    "Quick Setup: LTSP download fallback used ({})",
                                    e
                                ),
                            )
                        }
                    };
                    let _ = ui_tx.send(UiInternalMsg::FirstRunLtspFinished { ok, msg });
                    ctx_c.request_repaint();
                });
            } else {
                let plugin_svc =
                    crate::services::plugin_installer_service::PluginInstallerService::new(None);
                let _ = plugin_svc.ensure_luatools_js_installed(steam_root_owned.as_deref());
            }

            if self.first_run_restart_steam {
                self.trigger_restart_steam(ctx);
            }
        }

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

    fn render_overwrite_diff_modal(&mut self, ctx: &egui::Context) {
        if !self.overwrite_diff_modal.open {
            return;
        }
        let app_id = self.overwrite_diff_modal.app_id;
        let gname = self.resolve_game_title(app_id);
        let mut do_overwrite = false;
        let mut do_cancel = false;

        // Compute semantic diff (LT-32)
        let semantic_diff = compute_semantic_lua_diff(
            &self.overwrite_diff_modal.old_content,
            &self.overwrite_diff_modal.new_content,
            app_id,
        );

        let queued_remaining = self.pending_overwrite_diffs.len();
        let win_title = if queued_remaining > 0 {
            format!(
                "Confirm Overwrite — {} ({}.lua)  [+{} more queued]",
                gname, app_id, queued_remaining
            )
        } else {
            format!("Confirm Overwrite — {} ({}.lua)", gname, app_id)
        };

        egui::Window::new(win_title)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .default_size([600.0, 420.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(format!(
                    "An existing stplug-in/{}.lua was found with different contents. Review the semantic changes below before overwriting:",
                    app_id
                ))
                .size(12.5)
                .color(Palette::TEXT_SECONDARY),
            );
            ui.add_space(8.0);

            inset_paper_sheet(ui, Margin::same(10), |ui| {
                ui.set_width(ui.available_width());
                ScrollArea::vertical()
                    .id_salt("overwrite_diff_scroll")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;

                        // Summary badges
                        let mut summary_parts = Vec::new();
                        if !semantic_diff.added_depots.is_empty() {
                            summary_parts.push(format!("+{} depots", semantic_diff.added_depots.len()));
                        }
                        if !semantic_diff.removed_depots.is_empty() {
                            summary_parts.push(format!("-{} depots", semantic_diff.removed_depots.len()));
                        }
                        if !semantic_diff.added_manifest_pins.is_empty() {
                            summary_parts.push(format!("+{} manifest pins", semantic_diff.added_manifest_pins.len()));
                        }
                        if !semantic_diff.removed_manifest_pins.is_empty() {
                            summary_parts.push(format!("-{} manifest pins", semantic_diff.removed_manifest_pins.len()));
                        }
                        if !semantic_diff.added_depot_keys.is_empty() {
                            summary_parts.push(format!("+{} depot keys", semantic_diff.added_depot_keys.len()));
                        }
                        if !semantic_diff.changed_depot_keys.is_empty() {
                            summary_parts.push(format!("~{} depot keys", semantic_diff.changed_depot_keys.len()));
                        }
                        if !summary_parts.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                for part in summary_parts {
                                    let badge_color = if part.starts_with('+') { Palette::GO_TEXT }
                                        else if part.starts_with('-') { Palette::DANGER_TEXT }
                                        else { Palette::WARN_TEXT };
                                    ui.label(
                                        RichText::new(part)
                                            .size(11.0)
                                            .color(badge_color)
                                            .strong(),
                                    );
                                }
                            });
                            ui.add_space(8.0);
                        }

                        // Added depots
                        if !semantic_diff.added_depots.is_empty() {
                            ui.label(RichText::new("➕ Added Depots / DLCs:").size(12.0).color(Palette::GO_TEXT).strong());
                            for (depot_id, entry) in &semantic_diff.added_depots {
                                let comment = entry.comment.as_deref().unwrap_or("");
                                let key_str = entry.key.as_deref().map(|k| format!(" (key: {})", format_key_preview(k))).unwrap_or_default();
                                ui.label(
                                    RichText::new(format!("  addappid({}){} -- {}", depot_id, key_str, comment))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::GO_TEXT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Removed depots
                        if !semantic_diff.removed_depots.is_empty() {
                            ui.label(RichText::new("➖ Removed Depots / DLCs:").size(12.0).color(Palette::DANGER_TEXT).strong());
                            for depot_id in &semantic_diff.removed_depots {
                                ui.label(
                                    RichText::new(format!("  -- addappid({})", depot_id))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::DANGER_TEXT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Added manifest pins
                        if !semantic_diff.added_manifest_pins.is_empty() {
                            ui.label(RichText::new("📌 Added Manifest Pins:").size(12.0).color(Palette::BLUE_LIGHT).strong());
                            for (depot_id, gid) in &semantic_diff.added_manifest_pins {
                                ui.label(
                                    RichText::new(format!("  setManifestid({}, \"{}\")", depot_id, gid))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::BLUE_LIGHT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Removed manifest pins
                        if !semantic_diff.removed_manifest_pins.is_empty() {
                            ui.label(RichText::new("📍 Removed Manifest Pins:").size(12.0).color(Palette::WARN_TEXT).strong());
                            for depot_id in &semantic_diff.removed_manifest_pins {
                                ui.label(
                                    RichText::new(format!("  -- setManifestid({}, ...)", depot_id))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::WARN_TEXT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Changed depot keys
                        if !semantic_diff.changed_depot_keys.is_empty() {
                            ui.label(RichText::new("🔑 Changed Depot Keys:").size(12.0).color(Palette::WARN_TEXT).strong());
                            for (depot_id, (old_key, new_key)) in &semantic_diff.changed_depot_keys {
                                ui.label(
                                    RichText::new(format!("  ~ depot {}: {} → {}", depot_id, format_key_preview(old_key), format_key_preview(new_key)))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::WARN_TEXT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Added depot keys
                        if !semantic_diff.added_depot_keys.is_empty() {
                            ui.label(RichText::new("🔑 Added Depot Keys:").size(12.0).color(Palette::GO_TEXT).strong());
                            for (depot_id, key) in &semantic_diff.added_depot_keys {
                                ui.label(
                                    RichText::new(format!("  + depot {}: {}", depot_id, format_key_preview(key)))
                                        .font(egui::FontId::new(11.0, egui::FontFamily::Monospace))
                                        .color(Palette::GO_TEXT),
                                );
                            }
                            ui.add_space(4.0);
                        }

                        // Fallback: show raw line diff if no semantic changes detected
                        if semantic_diff.is_empty() && (!self.overwrite_diff_modal.removed_lines.is_empty() || !self.overwrite_diff_modal.added_lines.is_empty()) {
                            ui.label(RichText::new("Raw Line Diff (no semantic changes detected):").size(12.0).color(Palette::TEXT_MUTED));
                            for rem in &self.overwrite_diff_modal.removed_lines {
                                ui.label(
                                    RichText::new(rem)
                                        .font(egui::FontId::new(11.5, egui::FontFamily::Monospace))
                                        .color(Palette::DANGER_TEXT),
                                );
                            }
                            for add in &self.overwrite_diff_modal.added_lines {
                                ui.label(
                                    RichText::new(add)
                                        .font(egui::FontId::new(11.5, egui::FontFamily::Monospace))
                                        .color(Palette::GO_TEXT),
                                );
                            }
                        }
                    });
            });

            ui.add_space(12.0);
            egui::Sides::new().show(
                ui,
                |ui| {
                    if ghost_button(ui, "Cancel").clicked() {
                        do_cancel = true;
                    }
                },
                |ui| {
                    if primary_button(ui, "Overwrite (Backup to Vault)").clicked() {
                        do_overwrite = true;
                    }
                },
            );
        });

        if do_overwrite {
            self.lua_vault.sync_default_from_live(app_id);
            if let (Some(pkg), Some(installer)) = (
                self.overwrite_diff_modal.pending_package_path.clone(),
                self.lua_installer.clone(),
            ) {
                let res = installer.install_any(
                    &pkg,
                    Some(app_id),
                    false,
                    Some(&self.overwrite_diff_modal.source_label),
                    Some(&mut self.settings_service),
                    Some(&mut self.slssteam_service),
                );
                if self.overwrite_diff_modal.cleanup_parent {
                    if let Some(p) = pkg.parent() {
                        let _ = std::fs::remove_dir_all(p);
                    }
                }
                self.refresh_all_local_state();
                if res.ok {
                    self.notify(
                        format!("Overwrote {}.lua (previous version backed up in LuaVault).", app_id),
                        ToastKind::Success,
                    );
                } else if let Some(e) = res.error {
                    self.notify(e, ToastKind::Warning);
                }
            }
            self.overwrite_diff_modal.open = false;
            self.advance_overwrite_diff_queue();
        } else if do_cancel {
            if self.overwrite_diff_modal.cleanup_parent {
                if let Some(ref pkg) = self.overwrite_diff_modal.pending_package_path {
                    if let Some(p) = pkg.parent() {
                        let _ = std::fs::remove_dir_all(p);
                    }
                }
            }
            self.notify(format!("Skipped overwriting {}.lua.", app_id), ToastKind::Info);
            self.overwrite_diff_modal.open = false;
            self.advance_overwrite_diff_queue();
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
        let mut do_open_integration = false;
        let app_id = self.game_detail_modal.app_id;

        egui::Window::new(format!(
            "Edit .lua & Overrides — {} (#{})",
            self.game_detail_modal.game_name, app_id
        ))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .default_size([630.0, 490.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Edit this game's stplug-in .lua script, FakeAppId override, and optional AppToken.",
                )
                .size(12.0)
                .color(Palette::TEXT_MUTED),
            );
            ui.add_space(6.0);
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
                        if ghost_button(ui, "Set 480 (Spacewar)")
                            .on_hover_text("Use AppID 480 (Spacewar) for online multiplayer compatibility")
                            .clicked()
                        {
                            self.game_detail_modal.fake_id_input = "480".into();
                            self.game_detail_modal.feedback =
                                "Set FakeAppId to 480 — click Save .lua & Overrides to apply.".into();
                        }
                        if !self.game_detail_modal.fake_id_input.trim().is_empty()
                            && ghost_button(ui, "Clear").clicked()
                        {
                            self.game_detail_modal.fake_id_input.clear();
                            self.game_detail_modal.feedback =
                                "Cleared FakeAppId — click Save .lua & Overrides to apply.".into();
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
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if danger_button(ui, "Remove .lua")
                        .on_hover_text("Delete stplug-in/<appid>.lua and clear unlocker overrides for this game")
                        .clicked()
                    {
                        do_delete = true;
                    }
                    if secondary_button(ui, "Integration & Quick Setup…")
                        .on_hover_text("Switch to Integration & Process Controls (1-click Recommended Fixes, Play Unowned, Goldberg, LD_PRELOAD)")
                        .clicked()
                    {
                        do_open_integration = true;
                    }
                },
                |ui| {
                    if ghost_button(ui, "Cancel").clicked() {
                        close = true;
                    }
                    if primary_button(ui, "Save .lua & Overrides")
                        .on_hover_text("Write changes to stplug-in/<appid>.lua and sync SLSsteam config.yaml")
                        .clicked()
                    {
                        do_save = true;
                    }
                },
            );
        });

        if do_open_integration {
            self.game_detail_modal.open = false;
            self.open_integration_for_app(app_id);
            return;
        }

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
            self.launch_options_rebased_app = None;
            return;
        }
        let app_id = self.launch_options_modal.app_id;
        // LT-58: Run modal-open 3-way PICS baseline rebase once per modal open
        if self.launch_options_rebased_app != Some(app_id) {
            self.launch_options_rebased_app = Some(app_id);
            if let Some(root) = self.steam_service.installation.as_ref().map(|i| i.root.clone()) {
                let mut store = crate::services::launch_mod_store::LaunchModStore::new(None);
                let (rebased_entries, was_rebased) = store.rebase_on_modal_open(&root, app_id);
                if was_rebased && !rebased_entries.is_empty() {
                    self.launch_options_modal.selected_idx = Some(0);
                    self.launch_options_modal.entries = rebased_entries;
                    self.launch_options_modal.feedback =
                        "Rebased saved launch edits onto latest Steam PICS appinfo.vdf.".into();
                }
            }
        }
        let mut close = false;
        let mut do_save = false;
        let mut do_restore = false;

        let steam_running = self.launch_options_modal.steam_running;
        let is_busy = self.launch_options_modal.busy;
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
                    let banner_msg = if steam_running {
                        "Steam is currently running — saving or restoring will cleanly shut down Steam, update appinfo.vdf, and relaunch Steam via SLSsteam so changes aren't overwritten on exit."
                    } else {
                        "Steam is closed — changes will be written directly to appinfo.vdf and tracked for automatic PICS rebase."
                    };
                    ui.label(
                        RichText::new(banner_msg)
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
                let mut added_entry_idx: Option<usize> = None;
                let mut removed_entry_idx: Option<usize> = None;
                cols[0].horizontal(|ui| {
                    if secondary_button(ui, "+ Add").clicked() {
                        let next = self.launch_options_modal.entries.len();
                        self.launch_options_modal.entries.push(LaunchEntry {
                            index: next.to_string(),
                            launch_type: "default".into(),
                            ..Default::default()
                        });
                        self.launch_options_modal.selected_idx = Some(next);
                        added_entry_idx = Some(next);
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
                            removed_entry_idx = Some(sel);
                        }
                    }
                });
                if let Some(idx) = added_entry_idx {
                    self.launch_options_modal.feedback = format!(
                        "Added launch entry [{}] — edit fields and click Save Launch Options.",
                        idx
                    );
                } else if let Some(sel) = removed_entry_idx {
                    self.launch_options_modal.feedback = format!(
                        "Removed launch entry [{}] — click Save Launch Options to write.",
                        sel
                    );
                }

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
                                ui.label("Beta Key:");
                                let mut beta_buf = entry.beta_key.clone().unwrap_or_default();
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut beta_buf)
                                            .hint_text("Optional BetaKey"),
                                    )
                                    .changed()
                                {
                                    let t = beta_buf.trim();
                                    entry.beta_key = (!t.is_empty()).then(|| t.to_string());
                                }
                                ui.end_row();
                                ui.label("Owns DLC:");
                                let mut dlc_buf = entry.owns_dlc.clone().unwrap_or_default();
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut dlc_buf)
                                            .hint_text("Optional ownsdlc AppID"),
                                    )
                                    .changed()
                                {
                                    let t = dlc_buf.trim();
                                    entry.owns_dlc = (!t.is_empty()).then(|| t.to_string());
                                }
                                ui.end_row();
                            });
                    }
                }
            });

            if is_busy {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(0.55)
                        .text("Updating Steam appinfo.vdf…")
                        .animate(true)
                        .corner_radius(egui::CornerRadius::same(3)),
                );
            }
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
                    ui.add_enabled_ui(!is_busy, |ui| {
                        if secondary_button(ui, "Restore Backup").clicked() {
                            do_restore = true;
                        }
                    });
                },
                |ui| {
                    if ghost_button(ui, "Close").clicked() {
                        close = true;
                    }
                    ui.add_enabled_ui(!is_busy, |ui| {
                        let save_lbl = if is_busy {
                            "Saving…"
                        } else {
                            "Save Launch Options"
                        };
                        if primary_button(ui, save_lbl).clicked() {
                            do_save = true;
                        }
                    });
                },
            );
        });

        if do_save {
            let steam_root = self.steam_service.installation.as_ref().map(|i| i.root.clone());
            if let Some(root) = steam_root {
                self.launch_options_modal.busy = true;
                self.launch_options_modal.feedback =
                    "Updating Steam launch options in background…".into();
                self.notify("Updating Steam launch options…", ToastKind::Info);
                let entries = self.launch_options_modal.entries.clone();
                let tx = self.msg_tx.clone();
                let ctx_c = ctx.clone();
                thread::spawn(move || {
                    let mut store = crate::services::launch_mod_store::LaunchModStore::new(None);
                    match crate::services::launch_mod_store::save_launch_options_coordinated(
                        &root,
                        app_id,
                        &entries,
                        &mut store,
                    ) {
                        Ok(res) => {
                            let _ = tx.send(AppMessage::LaunchOptionsFinished {
                                app_id,
                                ok: true,
                                entries: Some(res.entries),
                                message: res.message,
                                warn: res.restart_error.is_some(),
                            });
                        }
                        Err(m) => {
                            let _ = tx.send(AppMessage::LaunchOptionsFinished {
                                app_id,
                                ok: false,
                                entries: None,
                                message: m,
                                warn: true,
                            });
                        }
                    }
                    ctx_c.request_repaint();
                });
            } else {
                self.launch_options_modal.feedback = "Steam installation not detected.".into();
                self.notify("Steam installation not detected.", ToastKind::Warning);
            }
        }
        if do_restore {
            let steam_root = self.steam_service.installation.as_ref().map(|i| i.root.clone());
            if let Some(root) = steam_root {
                self.launch_options_modal.busy = true;
                self.launch_options_modal.feedback =
                    "Restoring backup launch options in background…".into();
                self.notify("Restoring backup Steam launch options…", ToastKind::Info);
                let tx = self.msg_tx.clone();
                let ctx_c = ctx.clone();
                thread::spawn(move || {
                    let mut store = crate::services::launch_mod_store::LaunchModStore::new(None);
                    match crate::services::launch_mod_store::restore_launch_backup_coordinated(
                        &root,
                        app_id,
                        &mut store,
                    ) {
                        Ok(res) => {
                            let _ = tx.send(AppMessage::LaunchOptionsFinished {
                                app_id,
                                ok: true,
                                entries: Some(res.entries),
                                message: res.message,
                                warn: res.restart_error.is_some(),
                            });
                        }
                        Err(m) => {
                            let _ = tx.send(AppMessage::LaunchOptionsFinished {
                                app_id,
                                ok: false,
                                entries: None,
                                message: m,
                                warn: true,
                            });
                        }
                    }
                    ctx_c.request_repaint();
                });
            } else {
                self.launch_options_modal.feedback = "Steam installation not detected.".into();
                self.notify("Steam installation not detected.", ToastKind::Warning);
            }
        }
        if close {
            self.launch_options_modal.open = false;
            self.launch_options_rebased_app = None;
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
                        self.start_action_modal(
                            "auth",
                            "Discord Bot Code Sign-In",
                            "Redeeming 6-character Discord bot code…",
                            None,
                        );
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

    fn render_sudo_password_modal(&mut self, ctx: &egui::Context) {
        if !self.sudo_password_modal.open {
            return;
        }
        let mut submit_with_sudo = false;
        let mut install_without_sudo = false;
        let mut cancel = false;

        let title = if self.sudo_password_modal.title.is_empty() {
            "Administrator Password Required".to_string()
        } else {
            self.sudo_password_modal.title.clone()
        };

        egui::Window::new(title)
            .id(egui::Id::new("sudo_password_modal"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .fixed_size([490.0, 260.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.sudo_password_modal.reason)
                            .size(12.5)
                            .color(Palette::TEXT_SECONDARY),
                    )
                    .wrap_mode(egui::TextWrapMode::Wrap),
                );
                ui.add_space(10.0);

                ui.label(
                    RichText::new("Linux Administrator (sudo) Password")
                        .font(semibold_font(12.5))
                        .color(Palette::TEXT_PRIMARY),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let edit_w = (ui.available_width() - 68.0).max(200.0);
                    let resp = ui.add_sized(
                        [edit_w, 30.0],
                        TextEdit::singleline(&mut self.sudo_password_modal.password_input)
                            .password(!self.sudo_password_modal.show_password)
                            .hint_text("Enter your sudo password…"),
                    );
                    if self.sudo_password_modal.focus_requested {
                        resp.request_focus();
                        self.sudo_password_modal.focus_requested = false;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit_with_sudo = true;
                    }
                    let toggle_lbl = if self.sudo_password_modal.show_password {
                        "Hide"
                    } else {
                        "Show"
                    };
                    if ghost_button(ui, toggle_lbl).clicked() {
                        self.sudo_password_modal.show_password =
                            !self.sudo_password_modal.show_password;
                    }
                });

                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Used once in memory via SUDO_ASKPASS to install Millennium — never saved to disk or logged.",
                    )
                    .size(11.0)
                    .color(Palette::TEXT_MUTED),
                );

                if !self.sudo_password_modal.error_text.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(&self.sudo_password_modal.error_text)
                            .size(12.0)
                            .color(Palette::DANGER_TEXT),
                    );
                }

                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if primary_button(ui, "Install Unlocker + Millennium").clicked() {
                        submit_with_sudo = true;
                    }
                    if secondary_button(ui, "Unlocker Only (No Sudo)").clicked() {
                        install_without_sudo = true;
                    }
                    if ghost_button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if submit_with_sudo {
            if self.sudo_password_modal.password_input.is_empty() {
                self.sudo_password_modal.error_text =
                    "Enter your sudo password, or click 'Unlocker Only (No Sudo)'.".into();
                self.sudo_password_modal.focus_requested = true;
            } else {
                let pw = std::mem::take(&mut self.sudo_password_modal.password_input);
                self.sudo_password_modal.clear_secret();
                self.sudo_password_modal.error_text.clear();
                self.sudo_password_modal.open = false;
                self.run_wired_action_with_sudo("steam_unlocker", true, Some(pw), ctx);
            }
        } else if install_without_sudo {
            self.sudo_password_modal.clear_secret();
            self.sudo_password_modal.error_text.clear();
            self.sudo_password_modal.open = false;
            self.run_wired_action_with_sudo("steam_unlocker", false, None, ctx);
        } else if cancel {
            self.sudo_password_modal.clear_secret();
            self.sudo_password_modal.error_text.clear();
            self.sudo_password_modal.open = false;
        }
    }

    fn render_about_modal(&mut self, ctx: &egui::Context) {
        if !self.about_modal_open {
            return;
        }
        let mut close = false;
        let mut knock_title = false;
        let mut open_emblem = false;
        egui::Window::new(format!("About LuaTools Linux v{}", VERSION))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .fixed_size([490.0, 275.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let title_resp = ui.add(
                    egui::Label::new(
                        RichText::new(format!("LuaTools Linux  v{}", VERSION))
                            .font(display_font(19.0))
                            .color(Palette::TEXT_PRIMARY),
                    )
                    .sense(egui::Sense::click()),
                );
                if title_resp.clicked() {
                    knock_title = true;
                }
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
                egui::Sides::new().show(
                    ui,
                    |ui| {
                        let hint_resp = ui
                            .add(
                                egui::Label::new(
                                    RichText::new("Good luck.")
                                        .font(egui::FontId::new(
                                            10.5,
                                            egui::FontFamily::Monospace,
                                        ))
                                        .color(Palette::TEXT_MUTED),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Look behind the paper…");
                        if hint_resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if hint_resp.clicked() {
                            open_emblem = true;
                        }
                    },
                    |ui| {
                        if primary_button(ui, "Close").clicked() {
                            close = true;
                        }
                    },
                );
            });
        if knock_title {
            self.register_emblem_knock();
        }
        if open_emblem {
            self.about_modal_open = false;
            self.open_emblem_overlay();
        } else if close {
            self.about_modal_open = false;
        }
    }

    fn render_emblem_overlay(&mut self, ctx: &egui::Context) {
        if !self.emblem_overlay_open {
            return;
        }

        if self.emblem_overlay_texture.is_none() {
            if let Ok(img) = image::load_from_memory(PAPER_GRAIN_FALLBACK_BYTES) {
                let rgba = img.into_rgba8();
                let (w, h) = rgba.dimensions();
                let color_img = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    rgba.as_raw(),
                );
                let handle = ctx.load_texture(
                    "paper_grain_fallback_tex",
                    color_img,
                    egui::TextureOptions::LINEAR,
                );
                self.emblem_overlay_texture = Some(handle);
            }
        }

        let elapsed = self
            .emblem_overlay_opened_at
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(1.0);
        let fade = (elapsed / 0.28).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - fade).powi(3);
        if fade < 1.0 {
            ctx.request_repaint();
        }

        let screen = ctx.screen_rect();
        let can_click_dismiss = elapsed >= 0.28;
        let mut dismiss = false;

        egui::Area::new(egui::Id::new("emblem_overlay_surface"))
            .order(egui::Order::Tooltip)
            .fixed_pos(screen.min)
            .interactable(true)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
                if resp.hovered() && can_click_dismiss {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() && can_click_dismiss {
                    dismiss = true;
                }

                let p = ui.painter();
                let scrim_alpha = (236.0 * eased).round() as u8;
                p.rect_filled(
                    rect,
                    egui::CornerRadius::ZERO,
                    egui::Color32::from_rgba_unmultiplied(3, 4, 6, scrim_alpha),
                );

                if let Some(ref tex) = self.emblem_overlay_texture {
                    let native = tex.size_vec2();
                    let aspect = (native.x / native.y.max(1.0)).max(0.1);
                    let max_w = (screen.width() - 96.0).clamp(240.0, 660.0);
                    let max_h = (screen.height() - 130.0).clamp(180.0, 500.0);
                    let mut draw_w = max_w;
                    let mut draw_h = draw_w / aspect;
                    if draw_h > max_h {
                        draw_h = max_h;
                        draw_w = draw_h * aspect;
                    }

                    let plate_size = egui::Vec2::new(draw_w + 28.0, draw_h + 52.0);
                    let plate_rect = egui::Rect::from_center_size(screen.center(), plate_size);
                    let img_rect = egui::Rect::from_min_size(
                        egui::pos2(plate_rect.left() + 14.0, plate_rect.top() + 14.0),
                        egui::Vec2::new(draw_w, draw_h),
                    );

                    let shadow_alpha = (165.0 * eased).round() as u8;
                    p.rect_filled(
                        plate_rect.translate(egui::Vec2::new(4.0, 8.0)),
                        egui::CornerRadius::same(6),
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, shadow_alpha),
                    );
                    let plate_alpha = (255.0 * eased).round() as u8;
                    p.rect_filled(
                        plate_rect,
                        egui::CornerRadius::same(6),
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, plate_alpha),
                    );
                    p.rect_stroke(
                        plate_rect,
                        egui::CornerRadius::same(6),
                        egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(64, 70, 84, plate_alpha),
                        ),
                        egui::StrokeKind::Inside,
                    );

                    let tint = egui::Color32::from_rgba_unmultiplied(255, 255, 255, plate_alpha);
                    p.image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        tint,
                    );

                    let hint_alpha = (170.0 * eased).round() as u8;
                    p.text(
                        egui::pos2(plate_rect.center().x, plate_rect.bottom() - 18.0),
                        egui::Align2::CENTER_CENTER,
                        "Click anywhere or press Esc to return",
                        egui::FontId::new(11.5, egui::FontFamily::Monospace),
                        egui::Color32::from_rgba_unmultiplied(145, 152, 168, hint_alpha),
                    );
                }
            });

        if dismiss {
            self.close_emblem_overlay();
        }
    }

    pub fn open_integration_for_app(&mut self, app_id: u32) {
        let game_name = self.resolve_game_title(app_id);
        let install_path = self
            .library_rows
            .iter()
            .find(|r| r.app_id == app_id)
            .and_then(|r| r.install_path.clone())
            .or_else(|| self.steam_service.find_install_dir(app_id));
        self.open_integration_modal(app_id, game_name, install_path);
    }

    pub fn open_integration_modal(
        &mut self,
        app_id: u32,
        game_name: String,
        install_path: Option<std::path::PathBuf>,
    ) {
        let resolved_install_path = install_path
            .or_else(|| {
                self.library_rows
                    .iter()
                    .find(|r| r.app_id == app_id)
                    .and_then(|r| r.install_path.clone())
            })
            .or_else(|| self.steam_service.find_install_dir(app_id));
        let spoof = self
            .steam_bridge_service
            .get_app_id_spoof(app_id)
            .or_else(|| self.slssteam_service.get_fake_app_id(app_id))
            .unwrap_or(480)
            .to_string();
        let cd_key = self
            .steam_bridge_service
            .get_legacy_cd_key(app_id)
            .or_else(|| self.slssteam_service.get_cd_key(app_id).map(ToString::to_string))
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
        self.integration_modal.install_path = resolved_install_path;
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
        let cur_cdkey = self
            .steam_bridge_service
            .get_legacy_cd_key(app_id)
            .or_else(|| self.slssteam_service.get_cd_key(app_id).map(ToString::to_string));
        let cur_stats = self.steam_bridge_service.get_stats(app_id);

        let mut close = false;
        let mut jump_depots = false;
        let mut jump_edit_lua = false;
        let mut jump_launch_opts = false;
        let mut jump_fixes = false;
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
        let mut run_steamless = false;
        let mut clean_legacy = false;
        let mut open_folder_path: Option<std::path::PathBuf> = None;
        let mut browse_so_started = false;

        let screen_h = ctx.screen_rect().height();
        let modal_h = (screen_h - 104.0).clamp(400.0, 585.0);
        let scroll_max_h = (modal_h - 155.0).max(245.0);

        egui::Window::new(format!(
            "Integration & Process Controls — {} (#{})",
            game_name, app_id
        ))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .default_size([660.0, modal_h])
        .max_height(modal_h)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // Top 1-Click Game Tool Switcher so users never hunt through hidden menus
            inset_paper_sheet(ui, Margin::symmetric(12, 8), |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    ui.label(
                        RichText::new("Game Tools:")
                            .font(semibold_font(12.0))
                            .color(Palette::TEXT_MUTED),
                    );
                    if crate::ui::theme::table_button(ui, "Files & DLC", false)
                        .on_hover_text(
                            "Open Depots & Vault to toggle DLCs, switch saved .lua builds, pin manifests, or download depots",
                        )
                        .clicked()
                    {
                        jump_depots = true;
                    }
                    if crate::ui::theme::table_button(ui, "Edit .lua", false)
                        .on_hover_text(
                            "Edit stplug-in/<appid>.lua script, FakeAppId override, and AppToken",
                        )
                        .clicked()
                    {
                        jump_edit_lua = true;
                    }
                    if crate::ui::theme::table_button(ui, "Launch Options", false)
                        .on_hover_text(
                            "View or edit Steam appinfo.vdf launch executables and arguments",
                        )
                        .clicked()
                    {
                        jump_launch_opts = true;
                    }
                    if crate::ui::theme::table_button(ui, "Game Fixes", false)
                        .on_hover_text(
                            "Browse and apply verified DRM / online fixes with SHA-256 backup",
                        )
                        .clicked()
                    {
                        jump_fixes = true;
                    }
                    if let Some(ref p) = install_path {
                        if crate::ui::theme::table_button(ui, "Open Folder", false)
                            .on_hover_text("Open the local game installation folder in your file manager")
                            .clicked()
                        {
                            open_folder_path = Some(p.clone());
                        }
                    }
                });
            });

            ui.add_space(6.0);

            ScrollArea::vertical()
                .id_salt("integration_modal_scroll")
                .max_height(scroll_max_h)
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
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                            if primary_button(ui, "Apply Recommended Fixes Now")
                                .on_hover_text(
                                    "Enables Play-Unowned and applies Goldberg if installed locally (does not force Spacewar AppID 480)",
                                )
                                .clicked()
                            {
                                apply_quick_setup = true;
                            }
                            if install_path.is_some() {
                                if secondary_button(ui, "Remove SteamStub DRM (Steamless)")
                                    .on_hover_text(
                                        "Unpack SteamStub-wrapped game .exe files using Steamless",
                                    )
                                    .clicked()
                                {
                                    run_steamless = true;
                                }
                                if secondary_button(ui, "Clean Old Unlockers")
                                    .on_hover_text(
                                        "Remove leftover SmokeAPI/GreenLuma/Koalageddon files and restore original DLLs",
                                    )
                                    .clicked()
                                {
                                    clean_legacy = true;
                                }
                            }
                        });
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

                    // 3. Steam Stat Overrides (LNX-10: clearly gated as bridge-only experimental)
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(
                                    RichText::new("3. Steam Stat / Achievement Overrides")
                                        .font(semibold_font(13.5))
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            },
                            |ui| {
                                status_badge(ui, "Experimental · Bridge Only", BadgeTone::Warn);
                            },
                        );
                        ui.label(
                            RichText::new(
                                "Note: Stat overrides are stored for the experimental 64-bit bridge IPC store and are NOT read by 32-bit SLSsteam.so.",
                            )
                            .size(11.5)
                            .color(Palette::WARN_TEXT),
                        );
                        ui.checkbox(
                            &mut self.show_experimental_stat_overrides,
                            "Enable experimental bridge stat override controls",
                        );
                        if self.show_experimental_stat_overrides {
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
                        }
                    });

                    // 4. Game Process LD_PRELOAD Integration (LNX-10: verify actual .so presence on disk)
                    inset_paper_sheet(ui, Margin::same(12), |ui| {
                        ui.set_width(ui.available_width());
                        let lib_trimmed = self.integration_modal.proc_lib_input.trim();
                        let lib_exists = !lib_trimmed.is_empty()
                            && std::path::Path::new(lib_trimmed).is_file();
                        egui::Sides::new().show(
                            ui,
                            |ui| {
                                ui.label(
                                    RichText::new("4. Game Process Integration (LD_PRELOAD)")
                                        .font(semibold_font(13.5))
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            },
                            |ui| {
                                if lib_exists {
                                    status_badge(ui, "Library Found", BadgeTone::Ready);
                                } else {
                                    status_badge(ui, "Library Not Built / Missing", BadgeTone::Warn);
                                }
                            },
                        );
                        if !lib_exists {
                            ui.label(
                                RichText::new(
                                    "No compiled shared library (.so) was found at the path below (libgame_integration.so is not built by default). Point to an existing .so library before launching with LD_PRELOAD.",
                                )
                                .size(11.5)
                                .color(Palette::WARN_TEXT),
                            );
                        }
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
                                browse_so_started = true;
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
                            ui.add_enabled_ui(lib_exists, |ui| {
                                if primary_button(ui, "Launch with Integration")
                                    .on_disabled_hover_text(
                                        "Shared library (.so) does not exist at the specified path",
                                    )
                                    .clicked()
                                {
                                    save_proc = true;
                                    launch_proc = true;
                                }
                            });
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

        if let Some(p) = open_folder_path {
            match std::process::Command::new("xdg-open").arg(&p).spawn() {
                Ok(_) => self.notify(format!("Opened folder {}", p.display()), ToastKind::Info),
                Err(e) => self.notify(
                    format!("Failed to open folder {}: {}", p.display(), e),
                    ToastKind::Warning,
                ),
            }
        }
        if browse_so_started {
            self.notify("Opened file picker for shared library (.so)…", ToastKind::Info);
        }
        if let Some(enable) = toggle_redirect {
            if enable {
                self.steam_bridge_service.add_redirect_app(app_id);
                let _ = self
                    .play_unowned_service
                    .enable_for_app(app_id, &mut self.slssteam_service);
                self.integration_modal.feedback =
                    format!("Added AppID {} to play-unowned redirect set.", app_id);
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
            } else {
                self.steam_bridge_service.remove_redirect_app(app_id);
                let _ = self
                    .play_unowned_service
                    .disable_for_app(app_id, &mut self.slssteam_service);
                self.integration_modal.feedback =
                    format!("Removed AppID {} from redirect set.", app_id);
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
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
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
                }
                _ => {
                    self.integration_modal.feedback =
                        "Enter a valid positive numeric spoof AppID.".into();
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
                }
            }
        }
        if rem_spoof {
            self.steam_bridge_service.remove_app_id_spoof(app_id);
            self.slssteam_service.remove_fake_app_id(app_id);
            let _ = self.slssteam_service.save();
            self.refresh_all_local_state();
            self.integration_modal.feedback = format!("Removed AppID redirect for {}.", app_id);
            self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
        }
        if set_cdkey {
            let k = self.integration_modal.cd_key_input.trim().to_string();
            if !k.is_empty() {
                self.steam_bridge_service.set_legacy_cd_key(app_id, k.clone());
                self.slssteam_service.set_cd_key(app_id, k);
                let _ = self.slssteam_service.save();
                self.integration_modal.feedback =
                    "Saved legacy CD key (Bridge + SLSsteam config.yaml).".into();
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
            } else {
                self.integration_modal.feedback = "Enter a CD key before saving.".into();
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
            }
        }
        if rem_cdkey {
            self.steam_bridge_service.remove_legacy_cd_key(app_id);
            self.slssteam_service.remove_cd_key(app_id);
            let _ = self.slssteam_service.save();
            self.integration_modal.cd_key_input.clear();
            self.integration_modal.feedback = "Removed legacy CD key.".into();
            self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
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
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
                } else {
                    self.integration_modal.feedback =
                        "Depot key must be 64 hexadecimal characters.".into();
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
                }
            } else {
                self.integration_modal.feedback = "Enter a valid positive numeric Depot ID.".into();
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
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
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
                }
                _ => {
                    self.integration_modal.feedback =
                        "Enter a valid numeric Depot ID to remove.".into();
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
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
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
            } else {
                self.integration_modal.feedback = "Enter a stat name before saving.".into();
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
            }
        }
        if let Some(sname) = rem_stat {
            self.steam_bridge_service.remove_stat(app_id, &sname);
            self.integration_modal.feedback = format!("Removed stat '{}'.", sname);
            self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
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
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
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
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
                }
            } else {
                self.game_injection_service.unconfigure(app_id);
                self.integration_modal.feedback = "Disabled per-app process integration.".into();
                self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
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
        if run_steamless {
            if let Some(ref dir) = install_path {
                self.run_steamless_on_game_async(app_id, dir.clone(), ctx);
                self.integration_modal.feedback =
                    format!("Running Steamless on {}…", dir.display());
            }
        }
        if clean_legacy {
            if let Some(ref dir) = install_path {
                let report =
                    crate::services::mode_migration::ModeMigration::migrate_legacy_unlockers_in_game(
                        dir,
                    );
                if !report.errors.is_empty() {
                    self.integration_modal.feedback =
                        format!("Legacy cleanup errors: {}", report.errors.join("; "));
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Warning);
                } else if report.detected_files.is_empty() {
                    self.integration_modal.feedback =
                        "No legacy unlocker files (SmokeAPI/GreenLuma/BST) found.".into();
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Info);
                } else {
                    self.integration_modal.feedback = format!(
                        "Cleaned {} legacy unlocker file(s) and restored {} original DLL(s).",
                        report.removed_files.len(),
                        report.restored_files.len()
                    );
                    self.notify(self.integration_modal.feedback.clone(), ToastKind::Success);
                }
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

            // 2. Do NOT force Spacewar (480) FakeAppId here — setting 480 hijacks Steam
            // ownership tickets and causes 0 B install dialogs on normal games. Users can still
            // set a custom AppID spoof manually when using an Online-Fix multiplayer patch.

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
        if jump_depots {
            self.integration_modal.open = false;
            self.open_game_in_depots(app_id, ctx);
            return;
        }
        if jump_edit_lua {
            self.integration_modal.open = false;
            self.open_game_detail_modal(app_id, game_name);
            return;
        }
        if jump_launch_opts {
            self.integration_modal.open = false;
            self.open_launch_options_modal(app_id, game_name);
            return;
        }
        if jump_fixes {
            self.integration_modal.open = false;
            self.open_app_in_fixes(app_id, ctx);
            return;
        }
        if close {
            self.integration_modal.open = false;
        }
    }
}

/// Semantic diff between two .lua files (LT-32)
#[derive(Debug, Default)]
struct SemanticLuaDiff {
    added_depots: Vec<(u32, crate::parsers::lua_parser::LuaEntry)>,
    removed_depots: Vec<u32>,
    added_manifest_pins: Vec<(u32, String)>,
    removed_manifest_pins: Vec<u32>,
    added_depot_keys: Vec<(u32, String)>,
    changed_depot_keys: Vec<(u32, (String, String))>,
}

impl SemanticLuaDiff {
    fn is_empty(&self) -> bool {
        self.added_depots.is_empty()
            && self.removed_depots.is_empty()
            && self.added_manifest_pins.is_empty()
            && self.removed_manifest_pins.is_empty()
            && self.added_depot_keys.is_empty()
            && self.changed_depot_keys.is_empty()
    }
}

/// Compute semantic diff between two .lua files (LT-32)
fn compute_semantic_lua_diff(old: &str, new: &str, app_id: u32) -> SemanticLuaDiff {
    use crate::parsers::lua_parser::LuaFileParser;
    let old_parsed = LuaFileParser::parse_text(old, app_id);
    let new_parsed = LuaFileParser::parse_text(new, app_id);

    let old_depots: std::collections::HashMap<u32, &crate::parsers::lua_parser::LuaEntry> = old_parsed.entries.iter()
        .chain(old_parsed.disabled_entries.iter())
        .map(|e| (e.id, e))
        .collect();
    let new_depots: std::collections::HashMap<u32, &crate::parsers::lua_parser::LuaEntry> = new_parsed.entries.iter()
        .chain(new_parsed.disabled_entries.iter())
        .map(|e| (e.id, e))
        .collect();

    let mut added_depots = Vec::new();
    let mut removed_depots = Vec::new();
    let mut added_manifest_pins = Vec::new();
    let mut removed_manifest_pins = Vec::new();
    let mut added_depot_keys = Vec::new();
    let mut changed_depot_keys = Vec::new();

    for (id, new_entry) in &new_depots {
        if let Some(old_entry) = old_depots.get(id) {
            // Check for changed keys
            if let (Some(old_k), Some(new_k)) = (&old_entry.key, &new_entry.key) {
                if old_k != new_k {
                    changed_depot_keys.push((*id, (old_k.clone(), new_k.clone())));
                }
            }
            // Check for manifest pins
            if let Some(new_gid) = &new_entry.manifest_id {
                if old_entry.manifest_id.as_deref() != Some(new_gid.as_str()) {
                    added_manifest_pins.push((*id, new_gid.clone()));
                }
            }
            // Check for new keys
            if new_entry.key.is_some() && old_entry.key.is_none() {
                added_depot_keys.push((*id, new_entry.key.clone().unwrap()));
            }
        } else {
            // New depot
            added_depots.push((*id, (*new_entry).clone()));
        }
    }

    for (id, old_entry) in &old_depots {
        if !new_depots.contains_key(id) {
            removed_depots.push(*id);
        } else if let Some(old_gid) = &old_entry.manifest_id {
            if new_depots.get(id).and_then(|e| e.manifest_id.as_deref()) != Some(old_gid.as_str()) {
                removed_manifest_pins.push(*id);
            }
        }
    }

    SemanticLuaDiff {
        added_depots,
        removed_depots,
        added_manifest_pins,
        removed_manifest_pins,
        added_depot_keys,
        changed_depot_keys,
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

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use eframe::egui::{Pos2, RawInput, Rect, Vec2};

    #[test]
    fn emblem_overlay_lazy_decode_and_escape_dismiss() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());

        // Zero startup decode cost: texture must be None before trigger.
        assert!(!app.emblem_overlay_open);
        assert!(app.emblem_overlay_texture.is_none());

        // Five rapid knocks open the overlay.
        for _ in 0..5 {
            app.register_emblem_knock();
        }
        assert!(app.emblem_overlay_open);

        // First rendered frame lazily decodes the embedded asset.
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        let full = ctx.run(raw, |ctx| app.render_shell(ctx));
        assert!(!ctx.tessellate(full.shapes, 1.0).is_empty());
        assert!(app.emblem_overlay_texture.is_some());

        // Escape dismisses cleanly and frees the texture.
        let esc_raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(esc_raw, |ctx| app.render_shell(ctx));
        assert!(!app.emblem_overlay_open);
        assert!(app.emblem_overlay_texture.is_none());
    }

    #[test]
    fn sudo_password_modal_renders_and_clears_secret_on_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());

        app.sudo_password_modal.open = true;
        app.sudo_password_modal.title = "Administrator Password Required".into();
        app.sudo_password_modal.reason = "Millennium needs sudo".into();
        app.sudo_password_modal.password_input = "super-secret-pw".into();
        app.sudo_password_modal.show_password = true;

        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            ..Default::default()
        };
        let full = ctx.run(raw, |ctx| app.render_shell(ctx));
        assert!(!ctx.tessellate(full.shapes, 1.0).is_empty());
        assert!(app.sudo_password_modal.open);

        let esc_raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(esc_raw, |ctx| app.render_shell(ctx));
        assert!(!app.sudo_password_modal.open);
        assert!(
            app.sudo_password_modal.password_input.is_empty(),
            "Escape must clear sudo password buffer from memory"
        );
        assert!(!app.sudo_password_modal.show_password);
    }
}
