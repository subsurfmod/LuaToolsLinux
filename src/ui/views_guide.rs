//! Detailed "How to Use" Workspace (`ActiveView::Guide`) — interactive step-by-step
//! manual covering Quick Start, Manifest Ingest, Depots & LuaVault, Fixes & Launch Options,
//! Store Bridge, Troubleshooting, and Privacy-Safe Bug Reporting.

use eframe::egui::{
    self, Align, FontFamily, FontId, Layout, Margin, RichText, ScrollArea, TextEdit,
};
use egui_extras::{Column, TableBuilder};

use crate::ui::app::LuaToolsApp;
use crate::ui::state::{ActiveView, DepotsSubTab, GuideTopic, SettingsSubTab, ToastKind};
use crate::ui::theme::{
    chip_button, display_font, ghost_button, inset_paper_sheet, lua_layouter, paper_card,
    paper_card_padded, primary_button, secondary_button, section_header, semibold_font,
    status_badge, BadgeTone, Palette,
};

impl LuaToolsApp {
    pub fn render_guide(&mut self, ui: &mut egui::Ui) {
        let mut open_bug_report = false;
        let mut open_telemetry_settings = false;

        section_header(
            ui,
            "MANUAL",
            "How to Use LuaTools Linux",
            "Step-by-step reference for Steam manifests, LuaVault builds, depot downloads, game fixes, and troubleshooting",
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
                ("3. Depots, Vault & Downloads", GuideTopic::DepotsAndVault),
                ("4. Fixes, SLSsteam & Launch", GuideTopic::FixesAndOverrides),
                ("5. Bridge, FAQ & Bug Reports", GuideTopic::Troubleshooting),
            ];
            for (lbl, topic) in topics {
                if chip_button(ui, lbl, self.guide_topic == topic).clicked() {
                    self.guide_topic = topic;
                }
            }
        });

        ui.add_space(16.0);

        ScrollArea::vertical()
            .id_salt("how_to_use_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(920.0);
                match self.guide_topic {
                    GuideTopic::QuickStart => self.render_guide_quick_start(ui),
                    GuideTopic::AddingGames => self.render_guide_adding_games(ui),
                    GuideTopic::DepotsAndVault => self.render_guide_depots_and_vault(ui),
                    GuideTopic::FixesAndOverrides => self.render_guide_fixes_and_overrides(ui),
                    GuideTopic::Troubleshooting => self.render_guide_troubleshooting(ui),
                }
                ui.add_space(24.0);
            });
    }

    fn render_guide_quick_start(&mut self, ui: &mut egui::Ui) {
        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("How LuaTools Linux Works")
                    .font(display_font(18.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "LuaTools Linux manages Steam manifest configurations natively on Linux without Python or Qt overhead. \
                     Every game configuration coordinates four local locations on your machine:",
                )
                .size(13.0)
                .color(Palette::TEXT_SECONDARY),
            );
            ui.add_space(4.0);

            guide_bullet(
                ui,
                "~/.local/share/Steam/config/stplug-in/<appid>.lua",
                "Declares the base AppID, DLC entitlements, 64-character hex depot decryption keys, and optional pinned manifest GIDs.",
            );
            guide_bullet(
                ui,
                "~/.local/share/Steam/depotcache/<depot>_<gid>.manifest",
                "Binary Steam depot manifest snapshots extracted from .zip packages for offline or build-pinned installs.",
            );
            guide_bullet(
                ui,
                "~/.local/share/luatools/luavault/<appid>/",
                "Content-addressed SHA-256 vault storing every .lua build & preset so you can switch game versions in one click.",
            );
            guide_bullet(
                ui,
                "~/.config/SLSsteam/config.yaml",
                "Automatically synced with AdditionalApps, FakeAppIds (480 Spacewar), DepotKeys, AppTokens, and DlcData.",
            );
        });

        ui.add_space(14.0);

        let mut nav_target: Option<(ActiveView, Option<SettingsSubTab>)> = None;
        let mut open_create_modal = false;

        guide_step_card(
            ui,
            "STEP 01",
            "Verify Steam & Wired / SLSsteam Connection",
            "Check the top-right status pill ('● Steam Ready'). Under Settings › SLSsteam & Store Bridge, you can install \
             SLSsteam (~/.local/share/SLSsteam + ~/.local/bin/steam-sls) and ACCELA natively via the Wired Suite card, and enable \
             'PlayNotOwnedGames' (syncing play_unowned_games.lua) so unowned titles appear and launch in your Steam Library.",
            |ui| {
                if secondary_button(ui, "Open Steam Settings ›").clicked() {
                    nav_target = Some((ActiveView::Settings, Some(SettingsSubTab::General)));
                }
                if ghost_button(ui, "Wired & SLSsteam ›").clicked() {
                    nav_target = Some((ActiveView::Settings, Some(SettingsSubTab::Integrations)));
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "STEP 02",
            "Add Your First Game Manifest",
            "You can add a game in three ways:\n\
             • Drag and drop any .zip, .lua, or .manifest file directly onto the window (or click 'Select Files…' on Home).\n\
             • Open Discover (Ctrl+2), search a Steam title or paste a Store/SteamDB URL, sign in via Discord or Hubcap key, and click 'Install Manifest'.\n\
             • Click '+ Create by AppID' to generate a clean starter .lua from a template.",
            |ui| {
                if primary_button(ui, "Browse Discover ›").clicked() {
                    nav_target = Some((ActiveView::Acquire, None));
                }
                if secondary_button(ui, "+ Create by AppID").clicked() {
                    open_create_modal = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "STEP 03",
            "Restart Steam & Launch",
            "After installing or editing a .lua manifest or SLSsteam override, restart Steam (via Settings › General & Steam › 'Restart Steam' \
             when Millennium is active, or close and reopen Steam) so Steam reloads stplug-in and config.yaml.",
            |ui| {
                if secondary_button(ui, "View Configured Library ›").clicked() {
                    nav_target = Some((ActiveView::Library, None));
                }
            },
        );

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
        let mut nav_account = false;
        let mut focus_omnibox = false;

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("Three Ways to Add Games & Manifests")
                    .font(display_font(18.5))
                    .color(Palette::TEXT_PRIMARY),
            );

            guide_bullet(
                ui,
                "1. Drag & Drop / File Picker (Offline Packages)",
                "Drop <appid>.zip, <appid>.lua, <appid>_<buildid>.lua, or <depot>_<gid>.manifest anywhere on LuaTools. \
                 Archives are scanned with ZipSlip path-traversal protection: .lua files install to stplug-in/, .manifest files copy \
                 to depotcache/, a snapshot is stored in LuaVault, and SLSsteam config.yaml is updated automatically.",
            );
            guide_bullet(
                ui,
                "2. Cloud Manifest Providers (Discover)",
                "Sign in with Discord under Settings › Account & Providers (either via browser OAuth or a 6-character /login bot code), \
                 or paste a Hubcap API key (smm_ + 96 hex characters). Then select any game in Discover and click 'Install Manifest'. \
                 With 'FastFetch' enabled, the first available provider is pre-selected automatically.",
            );
            guide_bullet(
                ui,
                "3. Global Quick Command Bar (Ctrl+K / Ctrl+L)",
                "Press Ctrl+K from any tab and paste a numeric AppID (e.g. 1245620), a Steam Store URL, a SteamDB URL, \
                 a steam:// link, a luatools://install/<appid> URI, a local file path, or a game title to search.",
            );

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if primary_button(ui, "Open Discover").clicked() {
                    nav_discover = true;
                }
                if secondary_button(ui, "Account & Hubcap Sign-In").clicked() {
                    nav_account = true;
                }
                if ghost_button(ui, "Focus Command Bar (Ctrl+K)").clicked() {
                    focus_omnibox = true;
                }
            });
        });

        if nav_discover {
            self.active_view = ActiveView::Acquire;
        }
        if nav_account {
            self.active_view = ActiveView::Settings;
            self.settings_subtab = SettingsSubTab::Account;
        }
        if focus_omnibox {
            self.focus_command_next = true;
        }

        ui.add_space(14.0);

        paper_card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                RichText::new("Annotated stplug-in .lua Format Reference")
                    .font(display_font(17.5))
                    .color(Palette::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(
                    "Understanding each directive inside ~/.local/share/Steam/config/stplug-in/<appid>.lua:",
                )
                .size(12.5)
                .color(Palette::TEXT_MUTED),
            );

            let mut sample = String::from(
                "-- 1. Base game entitlement\n\
                 addappid(1245620) -- ELDEN RING\n\n\
                 -- 2. Encrypted content depot (64-char hex AES-256 depot key)\n\
                 addappid(1245621, 1, \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\") -- Core Game\n\
                 setManifestid(1245621, \"8492018394810293847\", 52428800000) -- Active pin locks this exact build\n\
                 -- setManifestid(1245622, \"1111222233334444555\", 2048) -- Commented pin allows Steam auto-updates\n\n\
                 -- 3. DLC entitlement (no key needed when content is shared in base depot)\n\
                 addappid(2778580) -- Shadow of the Erdtree\n\n\
                 -- 4. Optional app ownership token (synced to SLSsteam AppTokens)\n\
                 addtoken(1245620, \"YOUR_OPTIONAL_APP_TOKEN\")",
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
    }

    fn render_guide_depots_and_vault(&mut self, ui: &mut egui::Ui) {
        let mut go_depots: Option<DepotsSubTab> = None;

        guide_step_card(
            ui,
            "LUAVAULT",
            "Switching Game Builds & Saving Presets",
            "Every time a .lua file is installed or a <appid>_<buildid>.lua file is found in stplug-in/, LuaVault stores a \
             content-addressed SHA-256 copy under ~/.local/share/luatools/luavault/<appid>/.\n\
             • In Depots & Vault › Manifests & Vault, select any stored variant on the left and click 'Apply Selected Variant' \
             (or right-click to Apply, Rename, or Delete).\n\
             • Click '+ Save Preset' to snapshot your current configuration before experimenting with edits or downgrades.",
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
            "Pinned Builds vs Auto-Updating Games",
            "• Pinned (setManifestid active): Locks a depot to a specific manifest GID — ideal when a mod, translation, or game fix \
             requires a specific game patch.\n\
             • Auto-Update (-- setManifestid commented out): Lets Steam update to the newest public manifest. When 'Auto-update apps' \
             is enabled in Settings, standard installs comment out pins automatically while build-specific (<appid>_<buildid>.lua) \
             and fix manifests keep their pins locked.\n\
             • Toggle any individual depot pin in one click using the 'Pin' / 'Unpin' button in the Depot Declarations table.",
            |ui| {
                if ghost_button(ui, "Open Live Lua Editor ›").clicked() {
                    go_depots = Some(DepotsSubTab::LiveEditor);
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "DOWNLOADER",
            "Downloading Depots via DepotDownloader / DepotDownloaderMod",
            "In Depots & Vault › Download Depots, LuaTools automatically resolves official SteamRE/DepotDownloader (running via host \
             dotnet when installed on modern Linux distros) or a custom DepotDownloaderMod binary configured in Settings.\n\
             1. Choose an Output Folder and tick the content depots you want to download (shared redistributables and keyless DLC \
             entitlements are filtered out automatically).\n\
             2. For Steam accounts that own the depot, set your Steam Username in Settings › Account & Providers (or leave blank for QR) \
             and click 'Sign In (QR)' once to authenticate in your terminal with -remember-password.\n\
             3. Click 'Download Selected' — LuaTools supplies all known .lua and SLSsteam depot keys via a restricted (0600) temporary \
             keys file that is automatically deleted as soon as the download completes.",
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

        guide_step_card(
            ui,
            "OVERRIDES",
            "Library Registration (ACF), PlayUnowned & appinfo.vdf Launch Options",
            "In Library (Ctrl+3), every game offers:\n\
             • Configure: Edit SLSsteam FakeAppId (commonly 480 for Spacewar wrapper), set an AppToken, or edit stplug-in .lua.\n\
             • More › Register in Steam Library (ACF): Creates a valid \"AppState\" appmanifest_<appid>.acf, updates libraryfolders.vdf, \
             and syncs SLSsteam so 'Lua Only' games appear directly in your Steam Library.\n\
             • More › Enable Play Unowned / Run SteamAutoPatch: Syncs play_unowned_games.lua or launches Goldberg SteamAutoPatch.\n\
             • More › Launch Options…: Parses and writes binary Steam appcache/appinfo.vdf (v27–v29 with SHA-1 re-hashing and backup).\n\
             IMPORTANT: Always close Steam completely before writing appinfo.vdf or appmanifest_<appid>.acf files.",
            |ui| {
                if secondary_button(ui, "Open Library ›").clicked() {
                    go_library = true;
                }
            },
        );

        ui.add_space(12.0);

        guide_step_card(
            ui,
            "GAME FIXES",
            "Applying & Cleanly Reverting Game Fixes",
            "In Game Fixes (Ctrl+5), browse verified fixes by category or filter to 'Configured only':\n\
             • Install Manifest: Downloads and installs the build-locked manifest required by the fix.\n\
             • Apply Fix: Before touching any file in your game directory, LuaTools hashes existing files with SHA-256 and backs \
             them up into <GameDir>/.luatools-fix/backups/<fix_id>/ alongside a JSON ledger.\n\
             • Revert: Verifies post-apply SHA-256 hashes (preventing accidental corruption if another patch was layered on top), \
             deletes newly added fix files, and restores every original binary byte-for-byte.",
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
            "External Tools (DepotDownloaderMod, SteamAutoPatch & StubRemover)",
            "Under Settings › Updates & Toolchain, click 'Install / Update' on any external tool to download the latest release from GitHub:\n\
             • DepotDownloaderMod: Supports offline .manifest files (-manifestfile) paired with depot decryption keys.\n\
             • SteamAutoPatch (SteamAutoCrack): Applies Goldberg Steam emulator configurations to a selected game directory.\n\
             • StubRemover (Steamless): Unpacks SteamStub-protected .exe binaries via Wine (install Wine + .NET via ./install.sh --with-wine).",
            |ui| {
                if secondary_button(ui, "Open Toolchain ›").clicked() {
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
                "Open Settings › SLSsteam & Store Bridge, ensure 'PlayNotOwnedGames' is checked, click 'Save Changes', \
                 and fully restart Steam (not just minimize to tray).",
            );
            guide_bullet(
                ui,
                "Steam Store 'LuaTools' button is blocked by Linux Steam CSP",
                "Linux Steam's Content Security Policy blocks standard theme JS injection. Either use the native Discover tab directly, \
                 or enable 'Enable Steam CEF Debugging' + 'Enable CEF DevTools Store Bridge' in Settings › SLSsteam & Store Bridge and restart Steam.",
            );
            guide_bullet(
                ui,
                "DepotDownloader reports 'Access Denied' or 'No Subscription'",
                "Official SteamRE/DepotDownloader requires an authenticated Steam session for non-free depots — click 'Sign In (QR)' \
                 in Download Depots to cache your session, or install DepotDownloaderMod in Settings › Updates & Toolchain to use local .manifest files.",
            );
            guide_bullet(
                ui,
                "appinfo.vdf Launch Options disappeared after quitting Steam",
                "Steam flushes its in-memory appinfo cache to disk on exit. Run 'steam -shutdown' first, write Launch Options in LuaTools, \
                 and then start Steam. Use 'Restore Backup' anytime to revert to appinfo.vdf.luatools.bak.",
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
                    "Focus Global Quick Command & Search Bar",
                ),
                (
                    "Ctrl + 1 … Ctrl + 5",
                    "Switch to Home, Discover, Library, Depots & Vault, or Game Fixes",
                ),
                ("Ctrl + 6  /  F1", "Open this How to Use manual"),
                ("Ctrl + 7  /  Ctrl + ,", "Open Preferences & Settings"),
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
