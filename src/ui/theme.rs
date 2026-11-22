//! Sharp 0px Monotone + Cobalt Blue Theme, Widgets, Lua Syntax Highlighter & OS File Pickers.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily, FontId,
    Frame, Layout, Margin, RichText, Sense, Stroke, TextFormat, TextStyle, Ui, Vec2,
};
use regex::Regex;

pub struct Palette;

impl Palette {
    pub const BG_VOID: Color32 = Color32::from_rgb(9, 9, 11); // #09090B
    pub const BG_BASE: Color32 = Color32::from_rgb(14, 14, 17); // #0E0E11
    pub const BG_PANEL: Color32 = Color32::from_rgb(20, 20, 24); // #141418
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(27, 27, 33); // #1B1B21
    pub const BG_HOVER: Color32 = Color32::from_rgb(35, 35, 43); // #23232B
    pub const BG_ACTIVE: Color32 = Color32::from_rgb(43, 43, 53); // #2B2B35

    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(30, 30, 37); // #1E1E25
    pub const BORDER_DEFAULT: Color32 = Color32::from_rgb(43, 43, 52); // #2B2B34
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(62, 62, 75); // #3E3E4B

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(244, 244, 246); // #F4F4F6
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(161, 161, 170); // #A1A1AA
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(109, 109, 120); // #6D6D78

    pub const BLUE_PRIMARY: Color32 = Color32::from_rgb(37, 99, 235); // #2563EB
    pub const BLUE_HOVER: Color32 = Color32::from_rgb(59, 130, 246); // #3B82F6
    pub const BLUE_LIGHT: Color32 = Color32::from_rgb(96, 165, 250); // #60A5FA
    pub const BLUE_ICE: Color32 = Color32::from_rgb(147, 197, 253); // #93C5FD
    pub const BLUE_SUBTLE: Color32 = Color32::from_rgb(15, 26, 48); // #0F1A30
    pub const BLUE_BORDER: Color32 = Color32::from_rgb(29, 78, 216); // #1D4ED8

    pub const DANGER_TEXT: Color32 = Color32::from_rgb(248, 113, 113); // #F87171
    pub const DANGER_BG: Color32 = Color32::from_rgb(34, 17, 20); // #221114
    pub const DANGER_BORDER: Color32 = Color32::from_rgb(127, 29, 29); // #7F1D1D

    pub const WARN_TEXT: Color32 = Color32::from_rgb(251, 191, 36); // #FBBF24
    pub const WARN_BG: Color32 = Color32::from_rgb(35, 27, 14); // #231B0E
    pub const WARN_BORDER: Color32 = Color32::from_rgb(120, 53, 15); // #78350F
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    Blue,
    Neutral,
    Muted,
    Danger,
    Warn,
}

pub fn apply_sharp_monotone_theme(ctx: &egui::Context) {
    load_system_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.spacing.window_margin = Margin::same(16);

    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.0, FontFamily::Monospace),
    );
    style
        .text_styles
        .insert(TextStyle::Small, FontId::new(11.0, FontFamily::Monospace));
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(17.0, FontFamily::Proportional),
    );

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(Palette::TEXT_PRIMARY);
    v.panel_fill = Palette::BG_BASE;
    v.window_fill = Palette::BG_PANEL;
    v.extreme_bg_color = Palette::BG_VOID;
    v.faint_bg_color = Color32::from_rgb(17, 17, 21);
    v.code_bg_color = Palette::BG_VOID;
    v.hyperlink_color = Palette::BLUE_LIGHT;

    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.window_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);

    v.selection.bg_fill = Palette::BLUE_PRIMARY;
    v.selection.stroke = Stroke::new(1.0_f32, Palette::BLUE_LIGHT);

    // Non-interactive frames/borders
    v.widgets.noninteractive.bg_fill = Palette::BG_PANEL;
    v.widgets.noninteractive.weak_bg_fill = Palette::BG_PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_DEFAULT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    v.widgets.noninteractive.corner_radius = CornerRadius::ZERO;

    // Inactive controls
    v.widgets.inactive.bg_fill = Palette::BG_ELEVATED;
    v.widgets.inactive.weak_bg_fill = Palette::BG_ELEVATED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_DEFAULT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = CornerRadius::ZERO;

    // Hovered controls
    v.widgets.hovered.bg_fill = Palette::BG_HOVER;
    v.widgets.hovered.weak_bg_fill = Palette::BG_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_HOVER);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.hovered.corner_radius = CornerRadius::ZERO;
    v.widgets.hovered.expansion = 0.0;

    // Active / pressed controls
    v.widgets.active.bg_fill = Palette::BLUE_SUBTLE;
    v.widgets.active.weak_bg_fill = Palette::BLUE_SUBTLE;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_PRIMARY);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.active.corner_radius = CornerRadius::ZERO;
    v.widgets.active.expansion = 0.0;

    // Open combo/menus
    v.widgets.open.bg_fill = Palette::BG_PANEL;
    v.widgets.open.weak_bg_fill = Palette::BG_PANEL;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_HOVER);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.open.corner_radius = CornerRadius::ZERO;

    style.visuals = v;
    ctx.set_style(style);
}

fn load_system_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let sans_candidates = [
        "/usr/share/fonts/rsms-inter-fonts/InterVariable.ttf",
        "/usr/share/fonts/inter/Inter-Regular.ttf",
        "/usr/share/fonts/google-noto-vf/NotoSans[wght].ttf",
        "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
    ];
    for p in sans_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts
                .font_data
                .insert("sys_sans".into(), Arc::new(FontData::from_owned(bytes)));
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
                fam.insert(0, "sys_sans".into());
            }
            break;
        }
    }

    let mono_candidates = [
        "/usr/share/fonts/jetbrains-mono-fonts/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/adwaita-mono-fonts/AdwaitaMono-Regular.ttf",
        "/usr/share/fonts/liberation-mono-fonts/LiberationMono-Regular.ttf",
    ];
    for p in mono_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts
                .font_data
                .insert("sys_mono".into(), Arc::new(FontData::from_owned(bytes)));
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Monospace) {
                fam.insert(0, "sys_mono".into());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}

// ===== Reusable Sharp Architectural UI Primitives =====

pub fn panel_frame() -> Frame {
    Frame::new()
        .fill(Palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(14))
}

pub fn void_frame() -> Frame {
    Frame::new()
        .fill(Palette::BG_VOID)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(10))
}

pub fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(Color32::WHITE)
            .strong()
            .size(12.0),
    )
    .fill(Palette::BLUE_PRIMARY)
    .stroke(Stroke::new(1.0_f32, Palette::BLUE_HOVER))
    .corner_radius(CornerRadius::ZERO);
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(Palette::TEXT_PRIMARY)
            .strong()
            .size(12.0),
    )
    .fill(Palette::BG_ELEVATED)
    .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
    .corner_radius(CornerRadius::ZERO);
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn danger_button(ui: &mut Ui, label: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(Palette::DANGER_TEXT)
            .strong()
            .size(12.0),
    )
    .fill(Palette::DANGER_BG)
    .stroke(Stroke::new(1.0_f32, Palette::DANGER_BORDER))
    .corner_radius(CornerRadius::ZERO);
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn ghost_button(ui: &mut Ui, label: &str) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .color(Palette::TEXT_SECONDARY)
            .size(11.5),
    )
    .fill(Color32::TRANSPARENT)
    .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
    .corner_radius(CornerRadius::ZERO);
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn table_button(ui: &mut Ui, label: &str, primary: bool) -> egui::Response {
    let (fill, border, fg) = if primary {
        (
            Palette::BLUE_SUBTLE,
            Palette::BLUE_BORDER,
            Palette::BLUE_LIGHT,
        )
    } else {
        (
            Palette::BG_ELEVATED,
            Palette::BORDER_DEFAULT,
            Palette::TEXT_PRIMARY,
        )
    };
    let btn = egui::Button::new(RichText::new(label).color(fg).strong().size(11.0))
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::ZERO)
        .min_size(Vec2::new(48.0, 22.0));
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn chip_button(ui: &mut Ui, label: &str, active: bool) -> egui::Response {
    let (fill, border, fg) = if active {
        (
            Palette::BLUE_SUBTLE,
            Palette::BLUE_HOVER,
            Palette::BLUE_LIGHT,
        )
    } else {
        (
            Palette::BG_PANEL,
            Palette::BORDER_DEFAULT,
            Palette::TEXT_SECONDARY,
        )
    };
    let btn = egui::Button::new(
        RichText::new(label)
            .monospace()
            .strong()
            .size(11.0)
            .color(fg),
    )
    .fill(fill)
    .stroke(Stroke::new(1.0_f32, border))
    .corner_radius(CornerRadius::ZERO);
    let r = ui.add(btn);
    if r.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    r
}

pub fn nav_rail_button(ui: &mut Ui, label: &str, active: bool, width: f32) -> egui::Response {
    let desired = Vec2::new(width, 38.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if active {
            Palette::BLUE_SUBTLE
        } else if resp.hovered() {
            Color32::from_rgb(19, 19, 24)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, CornerRadius::ZERO, bg);

        let bar_color = if active {
            Palette::BLUE_HOVER
        } else if resp.hovered() {
            Palette::BORDER_STRONG
        } else {
            Color32::TRANSPARENT
        };
        let bar_rect = egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()));
        painter.rect_filled(bar_rect, CornerRadius::ZERO, bar_color);

        painter.line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            Stroke::new(1.0_f32, Color32::from_rgb(18, 18, 22)),
        );

        let text_color = if active || resp.hovered() {
            Color32::WHITE
        } else {
            Palette::TEXT_SECONDARY
        };
        painter.text(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::new(12.0, FontFamily::Monospace),
            text_color,
        );
    }
    resp
}

pub fn status_badge(ui: &mut Ui, text: &str, tone: BadgeTone) {
    let (bg, border, fg) = match tone {
        BadgeTone::Blue => (
            Palette::BLUE_SUBTLE,
            Palette::BLUE_BORDER,
            Palette::BLUE_LIGHT,
        ),
        BadgeTone::Neutral => (
            Palette::BG_ELEVATED,
            Palette::BORDER_DEFAULT,
            Palette::TEXT_SECONDARY,
        ),
        BadgeTone::Muted => (
            Palette::BG_VOID,
            Palette::BORDER_SUBTLE,
            Palette::TEXT_MUTED,
        ),
        BadgeTone::Danger => (
            Palette::DANGER_BG,
            Palette::DANGER_BORDER,
            Palette::DANGER_TEXT,
        ),
        BadgeTone::Warn => (Palette::WARN_BG, Palette::WARN_BORDER, Palette::WARN_TEXT),
    };
    Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text.to_ascii_uppercase())
                    .monospace()
                    .strong()
                    .size(10.5)
                    .color(fg),
            );
        });
}

pub fn section_header<F>(ui: &mut Ui, code: &str, title: &str, subtitle: &str, add_actions: F)
where
    F: FnOnce(&mut Ui),
{
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("// {}", code))
                        .monospace()
                        .strong()
                        .size(13.0)
                        .color(Palette::BLUE_HOVER),
                );
                ui.label(
                    RichText::new(title.to_ascii_uppercase())
                        .strong()
                        .size(17.0)
                        .color(Palette::TEXT_PRIMARY),
                );
            });
            if !subtitle.is_empty() {
                ui.label(
                    RichText::new(subtitle)
                        .size(12.0)
                        .color(Palette::TEXT_MUTED),
                );
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            add_actions(ui);
        });
    });
}

#[allow(clippy::too_many_arguments)]
pub fn metric_tile(
    ui: &mut Ui,
    category: &str,
    headline: &str,
    detail: &str,
    badge_text: &str,
    tone: BadgeTone,
    action_label: &str,
) -> bool {
    let mut clicked = false;
    let top_color = match tone {
        BadgeTone::Blue => Palette::BLUE_HOVER,
        BadgeTone::Danger => Palette::DANGER_TEXT,
        BadgeTone::Warn => Palette::WARN_TEXT,
        _ => Palette::BORDER_STRONG,
    };

    let resp = Frame::new()
        .fill(Palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_DEFAULT))
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(13))
        .show(ui, |ui| {
            ui.set_min_height(118.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(category.to_ascii_uppercase())
                            .monospace()
                            .strong()
                            .size(10.0)
                            .color(Palette::TEXT_MUTED),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        status_badge(ui, badge_text, tone);
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(headline)
                        .strong()
                        .size(14.5)
                        .color(Palette::TEXT_PRIMARY),
                );
                ui.label(
                    RichText::new(detail)
                        .size(11.5)
                        .color(Palette::TEXT_SECONDARY),
                );
                ui.add_space(6.0);
                if !action_label.is_empty() && ghost_button(ui, action_label).clicked() {
                    clicked = true;
                }
            });
        });

    let rect = resp.response.rect;
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(2.0_f32, top_color),
    );
    clicked
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "—".into();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx + 1 < units.len() {
        val /= 1024.0;
        idx += 1;
    }
    format!("{:.1} {}", val, units[idx])
}

// ===== Live Lua Syntax Highlighter for egui::TextEdit =====

pub fn lua_layouter(ui: &Ui, text: &str, wrap_width: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;

    let mono = FontId::new(12.0, FontFamily::Monospace);
    let default_fmt = TextFormat {
        font_id: mono.clone(),
        color: Palette::TEXT_PRIMARY,
        ..Default::default()
    };
    let comment_fmt = TextFormat {
        font_id: mono.clone(),
        color: Palette::TEXT_MUTED,
        italics: true,
        ..Default::default()
    };
    let fn_fmt = TextFormat {
        font_id: mono.clone(),
        color: Palette::BLUE_LIGHT,
        ..Default::default()
    };
    let str_fmt = TextFormat {
        font_id: mono.clone(),
        color: Palette::BLUE_ICE,
        ..Default::default()
    };

    let token_re = Regex::new(
        r#"(?i)(--[^\n]*)|("[^"]*"|'[^']*')|\b(addappid|setManifestid|addtoken|return|local|true|false)\b"#,
    )
    .unwrap();

    let mut last = 0usize;
    for m in token_re.find_iter(text) {
        if m.start() > last {
            job.append(&text[last..m.start()], 0.0, default_fmt.clone());
        }
        let slice = m.as_str();
        let fmt = if slice.starts_with("--") {
            comment_fmt.clone()
        } else if slice.starts_with('"') || slice.starts_with('\'') {
            str_fmt.clone()
        } else {
            fn_fmt.clone()
        };
        job.append(slice, 0.0, fmt);
        last = m.end();
    }
    if last < text.len() {
        job.append(&text[last..], 0.0, default_fmt);
    }

    ui.fonts(|f| f.layout_job(job))
}

// ===== Zero-C-Header Native Linux File/Folder Pickers (zenity / kdialog) =====

pub fn pick_files_native(title: &str, filter_glob: Option<&str>) -> Vec<PathBuf> {
    if let Ok(out) = Command::new("zenity")
        .args([
            "--file-selection",
            "--multiple",
            "--separator=\n",
            &format!("--title={}", title),
        ])
        .args(
            filter_glob
                .map(|g| vec![format!("--file-filter={}", g)])
                .unwrap_or_default(),
        )
        .output()
    {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        return Vec::new();
    }

    if let Ok(out) = Command::new("kdialog")
        .args(["--getopenfilename", ".", "--multiple", "--separate-output"])
        .output()
    {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
        }
    }
    Vec::new()
}

pub fn pick_directory_native(title: &str) -> Option<PathBuf> {
    if let Ok(out) = Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            &format!("--title={}", title),
        ])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
        return None;
    }

    if let Ok(out) = Command::new("kdialog")
        .args(["--getexistingdirectory", "."])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}
