//! Headless pure-Rust software rasterizer & visual layout QA test for LuaTools Linux.
//! Rasterizes `egui`'s tessellated `ClippedPrimitive` triangle meshes + font atlas directly
//! to PNG files when `LUATOOLS_QA_OUT` is set — zero X11/Wayland/GPU required.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{
    self,
    epaint::{ImageData, Primitive, TextureId},
    Color32, Pos2, RawInput, Rect, Vec2,
};

use luatools_linux::ui::app::LuaToolsApp;
use luatools_linux::ui::state::{ActiveView, DepotsSubTab, GuideTopic, SettingsSubTab, ToastKind};

struct SoftAtlas {
    width: usize,
    height: usize,
    pixels: Vec<Color32>,
}

impl SoftAtlas {
    fn sample(&self, uv: Pos2) -> Color32 {
        if self.width == 0 || self.height == 0 {
            return Color32::WHITE;
        }
        let fx =
            (uv.x.clamp(0.0, 1.0) * (self.width as f32) - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy = (uv.y.clamp(0.0, 1.0) * (self.height as f32) - 0.5)
            .clamp(0.0, (self.height - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = fx - (x0 as f32);
        let ty = fy - (y0 as f32);

        let p00 = self.pixels[y0 * self.width + x0].to_array();
        let p10 = self.pixels[y0 * self.width + x1].to_array();
        let p01 = self.pixels[y1 * self.width + x0].to_array();
        let p11 = self.pixels[y1 * self.width + x1].to_array();

        let mut out = [0u8; 4];
        for c in 0..4 {
            let top = (p00[c] as f32) * (1.0 - tx) + (p10[c] as f32) * tx;
            let bot = (p01[c] as f32) * (1.0 - tx) + (p11[c] as f32) * tx;
            out[c] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
        }
        Color32::from_rgba_premultiplied(out[0], out[1], out[2], out[3])
    }
}

fn update_textures(textures: &mut HashMap<TextureId, SoftAtlas>, delta: &egui::TexturesDelta) {
    for (tid, img_delta) in &delta.set {
        let [w, h] = img_delta.image.size();
        let new_px: Vec<Color32> = match &img_delta.image {
            ImageData::Color(c) => c.pixels.clone(),
            ImageData::Font(f) => f.srgba_pixels(None).collect(),
        };
        if let Some(pos) = img_delta.pos {
            if let Some(existing) = textures.get_mut(tid) {
                for row in 0..h {
                    let dy = pos[1] + row;
                    if dy >= existing.height {
                        break;
                    }
                    for col in 0..w {
                        let dx = pos[0] + col;
                        if dx < existing.width {
                            existing.pixels[dy * existing.width + dx] = new_px[row * w + col];
                        }
                    }
                }
            }
        } else {
            textures.insert(
                *tid,
                SoftAtlas {
                    width: w,
                    height: h,
                    pixels: new_px,
                },
            );
        }
    }
}

fn mul_color(a: Color32, b: Color32) -> [u8; 4] {
    let aa = a.to_array();
    let bb = b.to_array();
    [
        ((aa[0] as u32 * bb[0] as u32 + 127) / 255) as u8,
        ((aa[1] as u32 * bb[1] as u32 + 127) / 255) as u8,
        ((aa[2] as u32 * bb[2] as u32 + 127) / 255) as u8,
        ((aa[3] as u32 * bb[3] as u32 + 127) / 255) as u8,
    ]
}

fn blend_over(dst: &mut [u8; 4], src: [u8; 4]) {
    let inv_a = 255u32 - (src[3] as u32);
    for c in 0..4 {
        dst[c] = (src[c] as u32 + ((dst[c] as u32 * inv_a + 127) / 255)).min(255) as u8;
    }
}

fn edge_fn(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x)
}

fn rasterize_primitives(
    width: usize,
    height: usize,
    primitives: &[egui::ClippedPrimitive],
    textures: &HashMap<TextureId, SoftAtlas>,
) -> Vec<u8> {
    let mut fb = vec![[16u8, 17, 21, 255]; width * height];

    for cp in primitives {
        let Primitive::Mesh(mesh) = &cp.primitive else {
            continue;
        };
        let atlas = textures.get(&mesh.texture_id);
        let clip_min_x = (cp.clip_rect.min.x.floor().max(0.0) as usize).min(width);
        let clip_min_y = (cp.clip_rect.min.y.floor().max(0.0) as usize).min(height);
        let clip_max_x = (cp.clip_rect.max.x.ceil().max(0.0) as usize).min(width);
        let clip_max_y = (cp.clip_rect.max.y.ceil().max(0.0) as usize).min(height);
        if clip_min_x >= clip_max_x || clip_min_y >= clip_max_y {
            continue;
        }

        for &[i0, i1, i2] in mesh.indices.as_chunks::<3>().0 {
            let v0 = mesh.vertices[i0 as usize];
            let v1 = mesh.vertices[i1 as usize];
            let v2 = mesh.vertices[i2 as usize];

            let area = edge_fn(v0.pos, v1.pos, v2.pos);
            if area.abs() < 1e-4 {
                continue;
            }
            let inv_area = 1.0 / area;

            let min_x =
                (v0.pos.x.min(v1.pos.x).min(v2.pos.x).floor().max(0.0) as usize).max(clip_min_x);
            let max_x =
                (v0.pos.x.max(v1.pos.x).max(v2.pos.x).ceil().max(0.0) as usize).min(clip_max_x);
            let min_y =
                (v0.pos.y.min(v1.pos.y).min(v2.pos.y).floor().max(0.0) as usize).max(clip_min_y);
            let max_y =
                (v0.pos.y.max(v1.pos.y).max(v2.pos.y).ceil().max(0.0) as usize).min(clip_max_y);

            for y in min_y..max_y {
                let py = (y as f32) + 0.5;
                for x in min_x..max_x {
                    let p = Pos2::new((x as f32) + 0.5, py);
                    let w0 = edge_fn(v1.pos, v2.pos, p) * inv_area;
                    let w1 = edge_fn(v2.pos, v0.pos, p) * inv_area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 >= -1e-4 && w1 >= -1e-4 && w2 >= -1e-4 {
                        let uv = Pos2::new(
                            w0 * v0.uv.x + w1 * v1.uv.x + w2 * v2.uv.x,
                            w0 * v0.uv.y + w1 * v1.uv.y + w2 * v2.uv.y,
                        );
                        let c0 = v0.color.to_array();
                        let c1 = v1.color.to_array();
                        let c2 = v2.color.to_array();
                        let mut vcol = [0u8; 4];
                        for ch in 0..4 {
                            vcol[ch] = ((c0[ch] as f32) * w0
                                + (c1[ch] as f32) * w1
                                + (c2[ch] as f32) * w2)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                        }
                        let vcolor =
                            Color32::from_rgba_premultiplied(vcol[0], vcol[1], vcol[2], vcol[3]);
                        let tex_col = atlas.map(|a| a.sample(uv)).unwrap_or(Color32::WHITE);
                        let shaded = mul_color(vcolor, tex_col);
                        if shaded[3] > 0 {
                            blend_over(&mut fb[y * width + x], shaded);
                        }
                    }
                }
            }
        }
    }

    let mut raw = Vec::with_capacity(width * height * 4);
    for px in fb {
        // Un-premultiply for standard PNG RGBA8
        if px[3] == 0 {
            raw.extend_from_slice(&[0, 0, 0, 0]);
        } else if px[3] == 255 {
            raw.extend_from_slice(&px);
        } else {
            let a = px[3] as u32;
            raw.push(((px[0] as u32 * 255) / a).min(255) as u8);
            raw.push(((px[1] as u32 * 255) / a).min(255) as u8);
            raw.push(((px[2] as u32 * 255) / a).min(255) as u8);
            raw.push(px[3]);
        }
    }
    raw
}

#[test]
fn headless_papercut_views_render_cleanly_at_wide_and_narrow_resolutions() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());
    let mut textures: HashMap<TextureId, SoftAtlas> = HashMap::new();

    let out_dir_opt = std::env::var("LUATOOLS_QA_OUT").ok().map(PathBuf::from);
    if let Some(ref d) = out_dir_opt {
        std::fs::create_dir_all(d).unwrap();
    }

    struct QaViewCase {
        fname: &'static str,
        size: [usize; 2],
        view: ActiveView,
        d_sub: Option<DepotsSubTab>,
        s_sub: Option<SettingsSubTab>,
    }

    let cases: &[QaViewCase] = &[
        QaViewCase {
            fname: "01_home.png",
            size: [1380, 860],
            view: ActiveView::Overview,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "02_discover.png",
            size: [1380, 860],
            view: ActiveView::Acquire,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "03_library.png",
            size: [1380, 860],
            view: ActiveView::Library,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "04_depots_vault.png",
            size: [1380, 860],
            view: ActiveView::Depots,
            d_sub: Some(DepotsSubTab::Declarations),
            s_sub: None,
        },
        QaViewCase {
            fname: "05_depots_editor.png",
            size: [1380, 860],
            view: ActiveView::Depots,
            d_sub: Some(DepotsSubTab::LiveEditor),
            s_sub: None,
        },
        QaViewCase {
            fname: "06_depots_downloader.png",
            size: [1380, 860],
            view: ActiveView::Depots,
            d_sub: Some(DepotsSubTab::DownloadPicker),
            s_sub: None,
        },
        QaViewCase {
            fname: "07_fixes.png",
            size: [1380, 860],
            view: ActiveView::Fixes,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "08_settings_account.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::Account),
        },
        QaViewCase {
            fname: "09_settings_updates.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::UpdatesAndTools),
        },
        QaViewCase {
            fname: "10_narrow_1060x660.png",
            size: [1060, 660],
            view: ActiveView::Overview,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "11_toast_top_right.png",
            size: [1380, 860],
            view: ActiveView::Depots,
            d_sub: Some(DepotsSubTab::DownloadPicker),
            s_sub: None,
        },
        QaViewCase {
            fname: "12_how_to_use.png",
            size: [1380, 860],
            view: ActiveView::Guide,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "13_telemetry_popup.png",
            size: [1380, 860],
            view: ActiveView::Overview,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "14_settings_telemetry.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::Telemetry),
        },
        QaViewCase {
            fname: "15_settings_integrations_wired.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::Integrations),
        },
        QaViewCase {
            fname: "16_settings_steam_integration.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::SteamIntegration),
        },
        QaViewCase {
            fname: "17_depots_manifest_binding.png",
            size: [1380, 860],
            view: ActiveView::Depots,
            d_sub: Some(DepotsSubTab::ManifestBinding),
            s_sub: None,
        },
        QaViewCase {
            fname: "18_library_integration_modal.png",
            size: [1380, 860],
            view: ActiveView::Library,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "19_how_to_use_steam_interop.png",
            size: [1380, 860],
            view: ActiveView::Guide,
            d_sub: None,
            s_sub: None,
        },
        QaViewCase {
            fname: "20_settings_general.png",
            size: [1380, 860],
            view: ActiveView::Settings,
            d_sub: None,
            s_sub: Some(SettingsSubTab::General),
        },
    ];

    let guide_out_opt = std::env::var("LUATOOLS_GUIDE_OUT").ok().map(PathBuf::from);
    if let Some(ref gd) = guide_out_opt {
        std::fs::create_dir_all(gd).unwrap();
    }

    for case in cases {
        let fname = case.fname;
        let [w, h] = case.size;
        let view = case.view;
        let d_sub = case.d_sub;
        let s_sub = case.s_sub;
        app.telemetry_modal_open = fname == "13_telemetry_popup.png";
        app.guide_topic = if fname == "19_how_to_use_steam_interop.png" {
            GuideTopic::SteamInterop
        } else {
            GuideTopic::QuickStart
        };
        if fname == "18_library_integration_modal.png" {
            app.open_integration_modal(1245620, "ELDEN RING".into(), None);
        } else {
            app.integration_modal.open = false;
        }
        if fname == "09_settings_updates.png" {
            app.update_banner = luatools_linux::ui::state::UpdateBannerState::Available {
                badge: format!("Update v{}", luatools_linux::VERSION),
                tooltip: "New commits available on GitHub".into(),
            };
        } else {
            app.update_banner = luatools_linux::ui::state::UpdateBannerState::Hidden;
        }
        if fname == "11_toast_top_right.png" {
            app.notify(
                "Saved settings.json and synced Steam configuration",
                ToastKind::Success,
            );
        } else {
            app.active_toast = None;
        }
        app.active_view = view;
        if let Some(ds) = d_sub {
            app.depots_subtab = ds;
            if ds == DepotsSubTab::DownloadPicker {
                app.rebuild_depot_picks();
            }
        }
        if let Some(ss) = s_sub {
            app.settings_subtab = ss;
        }

        // Run 2 passes so egui multi-pass sizing (Sides / StripBuilder / Tables) settles completely
        let mut last_out = None;
        for _ in 0..2 {
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(w as f32, h as f32),
                )),
                ..Default::default()
            };
            let full = ctx.run(raw, |ctx| app.render_shell(ctx));
            update_textures(&mut textures, &full.textures_delta);
            last_out = Some(full);
        }
        let full = last_out.unwrap();
        let prims = ctx.tessellate(full.shapes, 1.0);
        assert!(!prims.is_empty(), "View {} produced zero primitives", fname);

        if let Some(ref out_dir) = out_dir_opt {
            let rgba = rasterize_primitives(w, h, &prims, &textures);
            image::save_buffer(
                out_dir.join(fname),
                &rgba,
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }

        if let Some(ref guide_dir) = guide_out_opt {
            let guide_target = match fname {
                "01_home.png" => Some("01_home_overview.png"),
                "02_discover.png" => Some("02_discover_store.png"),
                "04_depots_vault.png" => Some("03_depots_vault_binding.png"),
                "18_library_integration_modal.png" => Some("04_library_integration_modal.png"),
                "16_settings_steam_integration.png" => Some("05_settings_steam_integration.png"),
                "15_settings_integrations_wired.png" => Some("06_settings_wired_cloud.png"),
                _ => None,
            };
            if let Some(out_name) = guide_target {
                let rgba = rasterize_primitives(w, h, &prims, &textures);
                let src_img =
                    image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("valid rgba");
                let resized = image::imageops::resize(
                    &src_img,
                    960,
                    598,
                    image::imageops::FilterType::Lanczos3,
                );
                resized.save(guide_dir.join(out_name)).unwrap();
            }
        }
    }
}

#[test]
fn modals_render_cleanly_and_dismiss_with_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());

    let run_frame = |ctx: &egui::Context, app: &mut LuaToolsApp, send_esc: bool| {
        let mut events = Vec::new();
        if send_esc {
            events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0))),
            events,
            ..Default::default()
        };
        let full = ctx.run(raw, |ctx| app.render_shell(ctx));
        let prims = ctx.tessellate(full.shapes, 1.0);
        assert!(!prims.is_empty());
    };

    // 1. About modal renders and closes on Escape
    app.about_modal_open = true;
    run_frame(&ctx, &mut app, false);
    assert!(app.about_modal_open);
    run_frame(&ctx, &mut app, true);
    assert!(!app.about_modal_open);

    // 2. Add-by-AppID modal renders (even with u32::MAX text) and closes on Escape
    app.add_appid_modal.open = true;
    app.add_appid_modal.app_id_input = u32::MAX.to_string();
    app.add_appid_modal.template_idx = 2;
    run_frame(&ctx, &mut app, false);
    run_frame(&ctx, &mut app, true);
    assert!(!app.add_appid_modal.open);

    // 3. GameDetail modal renders with syntax-highlighted Lua and closes on Escape
    app.open_game_detail_modal(1245620, "ELDEN RING".into());
    app.game_detail_modal.lua_text = "addappid(1245620)\n".into();
    run_frame(&ctx, &mut app, false);
    assert!(app.game_detail_modal.open);
    run_frame(&ctx, &mut app, true);
    assert!(!app.game_detail_modal.open);

    // 4. LaunchOptions modal renders and closes on Escape
    app.open_launch_options_modal(1245620, "ELDEN RING".into());
    run_frame(&ctx, &mut app, false);
    assert!(app.launch_options_modal.open);
    run_frame(&ctx, &mut app, true);
    assert!(!app.launch_options_modal.open);

    // 5. DepotDetail modal renders with copy buttons and closes on Escape
    app.depot_detail_modal.open = true;
    app.depot_detail_modal.entry = app
        .depots_lua
        .as_ref()
        .and_then(|l| l.entries.first().cloned());
    run_frame(&ctx, &mut app, false);
    run_frame(&ctx, &mut app, true);
    assert!(!app.depot_detail_modal.open);

    // 6. TextPrompt modal renders and closes on Escape
    app.text_prompt_modal.open = true;
    app.text_prompt_modal.kind = "preset".into();
    app.text_prompt_modal.title = "Save Preset".into();
    app.text_prompt_modal.prompt = "Label:".into();
    run_frame(&ctx, &mut app, false);
    run_frame(&ctx, &mut app, true);
    assert!(!app.text_prompt_modal.open);

    // 7. Guide screenshot zoom modal renders and closes on Escape
    app.guide_zoom_screenshot = Some(("01_home_overview", "Home Workspace"));
    run_frame(&ctx, &mut app, false);
    run_frame(&ctx, &mut app, true);
    assert!(app.guide_zoom_screenshot.is_none());

    // 8. First-run telemetry consent modal must NOT be skipped by Escape
    app.settings_service.settings.telemetry_consent_seen = false;
    run_frame(&ctx, &mut app, true);
    assert!(!app.settings_service.settings.telemetry_consent_seen);
}

#[test]
fn cover_cooldown_prevents_retry_storms_and_protects_high_res_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = LuaToolsApp::new_headless_demo(&ctx, tmp.path());

    let aid = 999_999;
    let hi_url = "https://cdn.example.invalid/apps/999999/header.jpg".to_string();
    let lo_url = "https://cdn.example.invalid/apps/999999/capsule_sm_120.jpg".to_string();

    // Simulate a previous failure for the known high-res URL
    app.cover_known_urls.insert(aid, hi_url.clone());
    app.cover_failed_at.insert(aid, std::time::Instant::now());

    // Passing the same failed URL again within 45s must NOT re-trigger an in-flight fetch
    app.ensure_cover_requested(aid, Some(hi_url.clone()), &ctx);
    assert!(
        !app.cover_in_flight.contains(&aid),
        "Failed cover URL must respect 45s cooldown instead of spawning retry storms"
    );

    // Passing a low-res thumbnail URL must NOT downgrade the known high-res URL or bypass cooldown
    app.ensure_cover_requested(aid, Some(lo_url), &ctx);
    assert_eq!(app.cover_known_urls.get(&aid), Some(&hi_url));
    assert!(!app.cover_in_flight.contains(&aid));

    // When a fetch IS already in flight, passing the same URL again must NOT populate cover_retry_urls
    app.cover_in_flight.insert(aid);
    app.ensure_cover_requested(aid, Some(hi_url.clone()), &ctx);
    assert!(
        !app.cover_retry_urls.contains_key(&aid),
        "Identical URL must not queue a duplicate immediate retry"
    );

    // A genuinely new high-res URL while in flight SHOULD be queued for retry
    let newer_url = "https://cdn.example.invalid/apps/999999/header_v2.jpg".to_string();
    app.ensure_cover_requested(aid, Some(newer_url.clone()), &ctx);
    assert_eq!(app.cover_retry_urls.get(&aid), Some(&newer_url));
}

#[test]
fn lua_editor_widget_renders_with_validation_and_version_matches_release() {
    assert_eq!(luatools_linux::VERSION, "2.9.4-beta");

    let ctx = egui::Context::default();
    let mut editor = luatools_linux::ui::lua_editor::LuaEditor::new();
    editor.edit_live = true;
    editor.set_text(
        "addappid(1245620) -- ELDEN RING\naddappid(1245621, 1, \"shortkey\")\n".into(),
        1245620,
    );
    assert!(!editor.has_errors());
    assert!(editor
        .warnings
        .iter()
        .any(|w| w.contains("invalid decryption key")));

    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
        ..Default::default()
    };
    let full = ctx.run(raw, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ = editor.render(ui);
        });
    });
    let prims = ctx.tessellate(full.shapes, 1.0);
    assert!(!prims.is_empty());
}
