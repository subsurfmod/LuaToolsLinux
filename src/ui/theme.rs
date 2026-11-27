//! Tactile Monotone-Dark Papercut Design System & Reusable UI Primitives.
//!
//! Applies Papercut's Style DNA to a calm monotone-dark desktop workstation:
//! - Layered construction-paper surfaces (depth via cast shadow + stacked bottom shade lip + top cut-edge highlight, zero wireframe clutter)
//! - Darker sapphire-navy blue accents (earned saturation only on primary actions, active tags, and status signals)
//! - Rich multi-voice typography (STIX Two editorial serif display titles + Open Sans humanist UI + Adwaita/Hack code mono — zero Inter)
//! - Generous, consistent 8pt spacing rhythm (`8 / 16 / 24 px`)

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use eframe::egui::text::LayoutJob;
use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily, FontId, Frame,
    InnerResponse, Margin, Pos2, Rect, RichText, Sense, Stroke, TextFormat, TextStyle, Ui, Vec2,
};

pub struct Palette;

impl Palette {
    // Monotone Dark Paper Stack
    pub const BG_VOID: Color32 = Color32::from_rgb(12, 13, 16); // #0C0D10 recessed rail / header
    pub const BG_BASE: Color32 = Color32::from_rgb(16, 17, 21); // #101115 main desk canvas
    pub const BG_PANEL: Color32 = Color32::from_rgb(24, 26, 32); // #181A20 primary cut-paper card
    pub const BG_PANEL_LO: Color32 = Color32::from_rgb(10, 11, 14); // #0A0B0E stacked underside shade lip
    pub const BG_PANEL_HI: Color32 = Color32::from_rgb(39, 42, 51); // #272A33 top cut-edge highlight
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(31, 34, 42); // #1F222A secondary button / input
    pub const BG_HOVER: Color32 = Color32::from_rgb(39, 43, 53); // #272B35 hover paper lift
    pub const SHADOW: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 115);

    // Subtle Borders (used sparingly for inputs & dividers)
    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(32, 35, 43);
    pub const BORDER_DEFAULT: Color32 = Color32::from_rgb(44, 48, 58);
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(64, 70, 84);

    // Taped Paper Tag Strip (monotone warm bone paper + dark blue masking tape)
    pub const PAPER_CREAM: Color32 = Color32::from_rgb(218, 221, 228);
    pub const PAPER_CREAM_LO: Color32 = Color32::from_rgb(164, 169, 180);
    pub const PAPER_INK: Color32 = Color32::from_rgb(17, 19, 24);
    pub const TAPE_BLUE: Color32 = Color32::from_rgba_premultiplied(28, 62, 112, 210);

    // Typography
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(239, 240, 244); // #EFF0F4
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(166, 171, 182); // #A6ABB6
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(110, 115, 128); // #6E7380

    // Darker Sapphire-Navy Blue Accent (earned saturation)
    pub const BLUE_PRIMARY: Color32 = Color32::from_rgb(26, 58, 107); // #1A3A6B deep sapphire fill
    pub const BLUE_HOVER: Color32 = Color32::from_rgb(35, 75, 136); // #234B88 hover lift
    pub const BLUE_SHADE: Color32 = Color32::from_rgb(15, 34, 66); // #0F2242 3D paper bottom lip
    pub const BLUE_SUBTLE: Color32 = Color32::from_rgb(20, 33, 54); // #142136 active tint
    pub const BLUE_BORDER: Color32 = Color32::from_rgb(43, 84, 148); // #2B5494 top cut highlight
    pub const BLUE_LIGHT: Color32 = Color32::from_rgb(118, 162, 230); // #76A2E6 readable steel-blue text

    // Status Signals (each with dim paper twin)
    pub const GO_TEXT: Color32 = Color32::from_rgb(66, 196, 166);
    pub const GO_BG: Color32 = Color32::from_rgb(18, 43, 39);
    pub const DANGER_TEXT: Color32 = Color32::from_rgb(226, 98, 112);
    pub const DANGER_BG: Color32 = Color32::from_rgb(52, 22, 28);
    pub const DANGER_SHADE: Color32 = Color32::from_rgb(32, 12, 16);
    pub const DANGER_BORDER: Color32 = Color32::from_rgb(122, 42, 54);
    pub const WARN_TEXT: Color32 = Color32::from_rgb(228, 178, 86);
    pub const WARN_BG: Color32 = Color32::from_rgb(46, 36, 20);
    pub const WARN_BORDER: Color32 = Color32::from_rgb(112, 84, 34);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    Blue,
    Ready,
    Neutral,
    Muted,
    Danger,
    Warn,
}

pub fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Display".into()))
}

pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Semibold".into()))
}

pub fn apply_papercut_monotone_theme(ctx: &egui::Context) {
    load_rich_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.window_margin = Margin::same(20);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.indent = 16.0;
    style.spacing.interact_size = Vec2::new(44.0, 32.0);
    style.spacing.scroll = egui::style::ScrollStyle::solid();
    style.spacing.scroll.bar_width = 6.0;

    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(22.0, FontFamily::Name("Display".into())),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Name("Semibold".into())),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    let cut_radius = CornerRadius::same(4);
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(Palette::TEXT_PRIMARY);
    v.panel_fill = Palette::BG_BASE;
    v.window_fill = Palette::BG_PANEL;
    v.extreme_bg_color = Palette::BG_VOID;
    v.faint_bg_color = Color32::from_rgb(20, 22, 27);
    v.code_bg_color = Palette::BG_VOID;
    v.hyperlink_color = Palette::BLUE_LIGHT;

    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = cut_radius;
    v.window_stroke = Stroke::new(1.0_f32, Palette::BG_PANEL_HI);
    v.popup_shadow = egui::Shadow {
        offset: [3, 5],
        blur: 10,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.window_shadow = egui::Shadow {
        offset: [4, 8],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(160),
    };

    v.selection.bg_fill = Palette::BLUE_PRIMARY;
    v.selection.stroke = Stroke::new(1.0_f32, Palette::BLUE_LIGHT);

    v.widgets.noninteractive.bg_fill = Palette::BG_PANEL;
    v.widgets.noninteractive.weak_bg_fill = Palette::BG_PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    v.widgets.noninteractive.corner_radius = cut_radius;

    v.widgets.inactive.bg_fill = Palette::BG_VOID;
    v.widgets.inactive.weak_bg_fill = Palette::BG_ELEVATED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_DEFAULT);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = cut_radius;

    v.widgets.hovered.bg_fill = Palette::BG_HOVER;
    v.widgets.hovered.weak_bg_fill = Palette::BG_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_BORDER);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.hovered.corner_radius = cut_radius;
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = Palette::BLUE_SUBTLE;
    v.widgets.active.weak_bg_fill = Palette::BLUE_PRIMARY;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_LIGHT);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.active.corner_radius = cut_radius;
    v.widgets.active.expansion = 0.0;

    v.widgets.open.bg_fill = Palette::BG_PANEL;
    v.widgets.open.weak_bg_fill = Palette::BG_ELEVATED;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::BLUE_BORDER);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.open.corner_radius = cut_radius;

    style.visuals = v;
    ctx.set_style(style);
}

// Backwards-compat alias if referenced by tests
pub fn apply_sharp_monotone_theme(ctx: &egui::Context) {
    apply_papercut_monotone_theme(ctx);
}

fn load_rich_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // 1. Rich Humanist Sans Body (NOT Inter — Open Sans / Cantarell / Vazirmatn / Noto Sans)
    let body_candidates = [
        "/usr/share/fonts/open-sans/OpenSans-Regular.ttf",
        "/usr/share/fonts/truetype/open-sans/OpenSans-Regular.ttf",
        "/usr/share/fonts/abattis-cantarell-fonts/Cantarell-Regular.otf",
        "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let mut body_loaded = false;
    for p in body_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts
                .font_data
                .insert("rich_body".into(), Arc::new(FontData::from_owned(bytes)));
            for fam in fonts.families.values_mut() {
                fam.retain(|name| name != "Ubuntu-Light");
            }
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
                fam.insert(0, "rich_body".into());
            }
            fonts.font_data.remove("Ubuntu-Light");
            body_loaded = true;
            break;
        }
    }

    // 2. Real Semibold / Bold UI Weight for Buttons, Tabs & Headings
    let semibold_candidates = [
        "/usr/share/fonts/open-sans/OpenSans-Semibold.ttf",
        "/usr/share/fonts/open-sans/OpenSans-Bold.ttf",
        "/usr/share/fonts/truetype/open-sans/OpenSans-Semibold.ttf",
        "/usr/share/fonts/abattis-cantarell-fonts/Cantarell-Bold.otf",
        "/usr/share/fonts/google-noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/google-noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ];
    let mut semibold_chain = Vec::new();
    for p in semibold_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts.font_data.insert(
                "rich_semibold".into(),
                Arc::new(FontData::from_owned(bytes)),
            );
            semibold_chain.push("rich_semibold".into());
            break;
        }
    }
    if body_loaded {
        semibold_chain.push("rich_body".into());
    }
    if let Some(default_prop) = fonts.families.get(&FontFamily::Proportional).cloned() {
        semibold_chain.extend(default_prop);
    }
    fonts
        .families
        .insert(FontFamily::Name("Semibold".into()), semibold_chain.clone());

    // 3. Papercut Editorial Display Voice (STIX Two Text Bold / Caladea Bold / Noto Serif Bold)
    let display_candidates = [
        "/usr/share/fonts/stix-fonts/STIXTwoText-Bold.otf",
        "/usr/share/fonts/opentype/stix-word/STIXTwoText-Bold.otf",
        "/usr/share/fonts/google-crosextra-caladea-fonts/Caladea-Bold.ttf",
        "/usr/share/fonts/liberation-serif-fonts/LiberationSerif-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf",
    ];
    let mut display_chain = Vec::new();
    for p in display_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts
                .font_data
                .insert("rich_display".into(), Arc::new(FontData::from_owned(bytes)));
            display_chain.push("rich_display".into());
            break;
        }
    }
    display_chain.extend(semibold_chain);
    fonts
        .families
        .insert(FontFamily::Name("Display".into()), display_chain);

    // 4. Crisp Code Monospace (Adwaita Mono / JetBrains Mono / Hack / Liberation Mono)
    let mono_candidates = [
        "/usr/share/fonts/jetbrains-mono-fonts/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/adwaita-mono-fonts/AdwaitaMono-Regular.ttf",
        "/usr/share/fonts/source-foundry-hack-fonts/Hack-Regular.ttf",
        "/usr/share/fonts/liberation-mono-fonts/LiberationMono-Regular.ttf",
    ];
    for p in mono_candidates {
        if let Ok(bytes) = fs::read(p) {
            fonts
                .font_data
                .insert("rich_mono".into(), Arc::new(FontData::from_owned(bytes)));
            for fam in fonts.families.values_mut() {
                fam.retain(|name| name != "Hack");
            }
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Monospace) {
                fam.insert(0, "rich_mono".into());
            }
            fonts.font_data.remove("Hack");
            break;
        }
    }

    ctx.set_fonts(fonts);
}

// ===== Tactile Papercut Surface & Control Primitives =====

/// Layered construction-paper card: soft top-left-lit cast shadow, stacked darker bottom
/// paper lip, flat matte surface, and a 1px top cut-edge highlight (no wireframe borders).
pub fn paper_card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    paper_card_padded(ui, Margin::same(18), add_contents)
}

pub fn paper_card_padded<R>(
    ui: &mut Ui,
    padding: Margin,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let shape_idx = ui.painter().add(egui::Shape::Noop);
    let inner = Frame::new()
        .fill(Color32::TRANSPARENT)
        .inner_margin(padding)
        .show(ui, add_contents);

    let rect = inner.response.rect;
    let radius = CornerRadius::same(5);
    let shapes = vec![
        // 1. Cast drop shadow (+3, +4)
        egui::Shape::rect_filled(rect.translate(Vec2::new(3.0, 4.0)), radius, Palette::SHADOW),
        // 2. Stacked underside paper thickness lip (+0, +2)
        egui::Shape::rect_filled(
            rect.translate(Vec2::new(0.0, 2.0)),
            radius,
            Palette::BG_PANEL_LO,
        ),
        // 3. Flat matte paper body
        egui::Shape::rect_filled(rect, radius, Palette::BG_PANEL),
        // 4. Top cut-edge highlight sliver
        egui::Shape::line_segment(
            [
                Pos2::new(rect.left() + 4.0, rect.top() + 0.5),
                Pos2::new(rect.right() - 4.0, rect.top() + 0.5),
            ],
            Stroke::new(1.0_f32, Palette::BG_PANEL_HI),
        ),
    ];
    ui.painter().set(shape_idx, egui::Shape::Vec(shapes));
    inner
}

/// Recessed dark inset sheet for code editors, logs, and inner lists.
pub fn inset_paper_sheet<R>(
    ui: &mut Ui,
    padding: Margin,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    Frame::new()
        .fill(Palette::BG_VOID)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(padding)
        .show(ui, add_contents)
}

pub fn panel_frame() -> Frame {
    Frame::new()
        .fill(Palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, Palette::BG_PANEL_HI))
        .corner_radius(CornerRadius::same(5))
        .shadow(egui::Shadow {
            offset: [2, 4],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(100),
        })
        .inner_margin(Margin::same(18))
}

pub fn void_frame() -> Frame {
    Frame::new()
        .fill(Palette::BG_VOID)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::same(12))
}

#[derive(Clone, Copy)]
struct CutButtonStyle {
    face: Color32,
    face_hover: Color32,
    shade_lip: Color32,
    top_hi: Color32,
    text_col: Color32,
    min_h: f32,
    pad_x: f32,
    font_sz: f32,
}

fn draw_cut_paper_button(ui: &mut Ui, label: &str, style: CutButtonStyle) -> egui::Response {
    let font_id = semibold_font(style.font_sz);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, style.text_col);
    let desired = Vec2::new(
        (galley.size().x + style.pad_x * 2.0).max(52.0),
        (galley.size().y + 12.0).max(style.min_h),
    );
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() && ui.is_enabled() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let press_y = if resp.is_pointer_button_down_on() && enabled {
            1.5
        } else {
            0.0
        };
        let radius = CornerRadius::same(4);
        let painter = ui.painter();

        // Soft cast shadow + stacked darker bottom edge
        if enabled {
            painter.rect_filled(
                rect.translate(Vec2::new(1.5, 3.0)),
                radius,
                Color32::from_black_alpha(95),
            );
            painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, style.shade_lip);
        }

        let active_face = if !enabled {
            Color32::from_rgb(24, 26, 32)
        } else if resp.hovered() {
            style.face_hover
        } else {
            style.face
        };
        let face_rect = rect.translate(Vec2::new(0.0, press_y));
        painter.rect_filled(face_rect, radius, active_face);

        if enabled {
            painter.line_segment(
                [
                    Pos2::new(face_rect.left() + 3.0, face_rect.top() + 0.5),
                    Pos2::new(face_rect.right() - 3.0, face_rect.top() + 0.5),
                ],
                Stroke::new(1.0_f32, style.top_hi),
            );
        }

        let fg = if enabled {
            style.text_col
        } else {
            Palette::TEXT_MUTED
        };
        let text_pos = Pos2::new(
            face_rect.center().x - galley.size().x * 0.5,
            face_rect.center().y - galley.size().y * 0.5,
        );
        painter.galley(text_pos, galley, fg);
    }
    resp
}

pub fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    draw_cut_paper_button(
        ui,
        label,
        CutButtonStyle {
            face: Palette::BLUE_PRIMARY,
            face_hover: Palette::BLUE_HOVER,
            shade_lip: Palette::BLUE_SHADE,
            top_hi: Palette::BLUE_BORDER,
            text_col: Palette::TEXT_PRIMARY,
            min_h: 32.0,
            pad_x: 15.0,
            font_sz: 12.5,
        },
    )
}

pub fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    draw_cut_paper_button(
        ui,
        label,
        CutButtonStyle {
            face: Palette::BG_ELEVATED,
            face_hover: Palette::BG_HOVER,
            shade_lip: Palette::BG_PANEL_LO,
            top_hi: Palette::BORDER_DEFAULT,
            text_col: Palette::TEXT_PRIMARY,
            min_h: 32.0,
            pad_x: 14.0,
            font_sz: 12.5,
        },
    )
}

pub fn danger_button(ui: &mut Ui, label: &str) -> egui::Response {
    draw_cut_paper_button(
        ui,
        label,
        CutButtonStyle {
            face: Palette::DANGER_BG,
            face_hover: Color32::from_rgb(68, 28, 36),
            shade_lip: Palette::DANGER_SHADE,
            top_hi: Palette::DANGER_BORDER,
            text_col: Palette::DANGER_TEXT,
            min_h: 32.0,
            pad_x: 14.0,
            font_sz: 12.5,
        },
    )
}

pub fn ghost_button(ui: &mut Ui, label: &str) -> egui::Response {
    let font_id = semibold_font(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Palette::TEXT_SECONDARY);
    let desired = Vec2::new((galley.size().x + 22.0).max(44.0), 30.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() && ui.is_enabled() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(4), Palette::BG_ELEVATED);
        } else {
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(1.0_f32, Palette::BORDER_SUBTLE),
                egui::StrokeKind::Inside,
            );
        }
        let col = if resp.hovered() {
            Palette::TEXT_PRIMARY
        } else {
            Palette::TEXT_SECONDARY
        };
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            col,
        );
    }
    resp
}

pub fn table_button(ui: &mut Ui, label: &str, primary: bool) -> egui::Response {
    if primary {
        draw_cut_paper_button(
            ui,
            label,
            CutButtonStyle {
                face: Palette::BLUE_PRIMARY,
                face_hover: Palette::BLUE_HOVER,
                shade_lip: Palette::BLUE_SHADE,
                top_hi: Palette::BLUE_BORDER,
                text_col: Palette::TEXT_PRIMARY,
                min_h: 26.0,
                pad_x: 11.0,
                font_sz: 11.5,
            },
        )
    } else {
        draw_cut_paper_button(
            ui,
            label,
            CutButtonStyle {
                face: Palette::BG_ELEVATED,
                face_hover: Palette::BG_HOVER,
                shade_lip: Palette::BG_PANEL_LO,
                top_hi: Palette::BORDER_DEFAULT,
                text_col: Palette::TEXT_SECONDARY,
                min_h: 26.0,
                pad_x: 10.0,
                font_sz: 11.5,
            },
        )
    }
}

pub fn chip_button(ui: &mut Ui, label: &str, active: bool) -> egui::Response {
    let font_id = semibold_font(12.0);
    let fg = if active {
        Palette::TEXT_PRIMARY
    } else {
        Palette::TEXT_SECONDARY
    };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font_id, fg);
    let desired = Vec2::new(galley.size().x + 22.0, 29.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let r = CornerRadius::same(4);
        if active {
            painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), r, Palette::BLUE_SHADE);
            painter.rect_filled(rect, r, Palette::BLUE_PRIMARY);
            painter.line_segment(
                [
                    Pos2::new(rect.left() + 3.0, rect.top() + 0.5),
                    Pos2::new(rect.right() - 3.0, rect.top() + 0.5),
                ],
                Stroke::new(1.0_f32, Palette::BLUE_BORDER),
            );
        } else {
            let bg = if resp.hovered() {
                Palette::BG_HOVER
            } else {
                Palette::BG_PANEL
            };
            painter.rect_filled(rect.translate(Vec2::new(0.0, 1.5)), r, Palette::BG_PANEL_LO);
            painter.rect_filled(rect, r, bg);
        }
        painter.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            fg,
        );
    }
    resp
}

pub fn nav_rail_button(
    ui: &mut Ui,
    label: &str,
    badge_count: Option<usize>,
    active: bool,
    width: f32,
) -> egui::Response {
    let desired = Vec2::new(width, 38.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    if ui.is_rect_visible(rect) {
        let inner = rect.shrink2(Vec2::new(10.0, 2.0));
        let painter = ui.painter();
        let r = CornerRadius::same(4);

        if active {
            painter.rect_filled(
                inner.translate(Vec2::new(1.5, 2.5)),
                r,
                Color32::from_black_alpha(90),
            );
            painter.rect_filled(inner, r, Palette::BLUE_SUBTLE);
            let tab_bar = Rect::from_min_size(
                Pos2::new(inner.left(), inner.top() + 5.0),
                Vec2::new(3.5, inner.height() - 10.0),
            );
            painter.rect_filled(tab_bar, CornerRadius::same(2), Palette::BLUE_LIGHT);
        } else if resp.hovered() {
            painter.rect_filled(inner, r, Palette::BG_PANEL);
        }

        let text_color = if active {
            Palette::TEXT_PRIMARY
        } else if resp.hovered() {
            Color32::from_rgb(215, 218, 226)
        } else {
            Palette::TEXT_SECONDARY
        };
        let font = if active {
            semibold_font(13.5)
        } else {
            FontId::new(13.5, FontFamily::Proportional)
        };
        painter.text(
            Pos2::new(inner.left() + 14.0, inner.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            text_color,
        );

        if let Some(n) = badge_count.filter(|&c| c > 0) {
            let pill_txt = n.to_string();
            let g = painter.layout_no_wrap(
                pill_txt,
                FontId::new(11.0, FontFamily::Monospace),
                if active {
                    Palette::BLUE_LIGHT
                } else {
                    Palette::TEXT_MUTED
                },
            );
            let pw = (g.size().x + 12.0).max(22.0);
            let pill_rect = Rect::from_center_size(
                Pos2::new(inner.right() - pw * 0.5 - 8.0, inner.center().y),
                Vec2::new(pw, 19.0),
            );
            painter.rect_filled(
                pill_rect,
                CornerRadius::same(3),
                if active {
                    Palette::BLUE_PRIMARY
                } else {
                    Palette::BG_PANEL
                },
            );
            painter.galley(
                Pos2::new(
                    pill_rect.center().x - g.size().x * 0.5,
                    pill_rect.center().y - g.size().y * 0.5,
                ),
                g,
                Palette::TEXT_PRIMARY,
            );
        }
    }
    resp
}

pub fn status_badge(ui: &mut Ui, text: &str, tone: BadgeTone) {
    let (bg, fg) = match tone {
        BadgeTone::Blue => (Palette::BLUE_SUBTLE, Palette::BLUE_LIGHT),
        BadgeTone::Ready => (Palette::GO_BG, Palette::GO_TEXT),
        BadgeTone::Neutral => (Palette::BG_ELEVATED, Palette::TEXT_SECONDARY),
        BadgeTone::Muted => (Palette::BG_VOID, Palette::TEXT_MUTED),
        BadgeTone::Danger => (Palette::DANGER_BG, Palette::DANGER_TEXT),
        BadgeTone::Warn => (Palette::WARN_BG, Palette::WARN_TEXT),
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), semibold_font(11.0), fg);
    let desired = Vec2::new(galley.size().x + 16.0, 22.0);
    let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        ui.painter().galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            fg,
        );
    }
}

/// Papercut Taped Section Header:
/// Renders a small tactile cream-paper tag with a dark-blue masking-tape tab,
/// rich editorial Display serif title, calm sentence-case subtitle, and non-overlapping
/// right-side actions via `egui::Sides`.
pub fn section_header<F>(ui: &mut Ui, tag_label: &str, title: &str, subtitle: &str, add_actions: F)
where
    F: FnOnce(&mut Ui),
{
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    draw_taped_paper_tag(ui, tag_label);
                    ui.label(
                        RichText::new(title)
                            .font(display_font(22.0))
                            .color(Palette::TEXT_PRIMARY),
                    );
                });
                if !subtitle.is_empty() {
                    ui.label(
                        RichText::new(subtitle)
                            .size(13.0)
                            .color(Palette::TEXT_MUTED),
                    );
                }
            });
        },
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                add_actions(ui);
            });
        },
    );
    ui.add_space(12.0);
}

pub fn draw_taped_paper_tag(ui: &mut Ui, text: &str) {
    let galley = ui.painter().layout_no_wrap(
        text.to_ascii_uppercase(),
        semibold_font(10.0),
        Palette::PAPER_INK,
    );
    let desired = Vec2::new(galley.size().x + 18.0, 20.0);
    let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let r = CornerRadius::same(2);
        p.rect_filled(
            rect.translate(Vec2::new(1.5, 2.5)),
            r,
            Color32::from_black_alpha(110),
        );
        p.rect_filled(
            rect.translate(Vec2::new(0.0, 1.5)),
            r,
            Palette::PAPER_CREAM_LO,
        );
        p.rect_filled(rect, r, Palette::PAPER_CREAM);
        // Translucent blue masking-tape tab on the left edge
        let tape_rect = Rect::from_min_size(
            Pos2::new(rect.left() - 3.0, rect.top() + 3.0),
            Vec2::new(7.0, rect.height() - 6.0),
        );
        p.rect_filled(tape_rect, CornerRadius::same(1), Palette::TAPE_BLUE);
        p.galley(
            Pos2::new(
                rect.center().x - galley.size().x * 0.5 + 1.0,
                rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            Palette::PAPER_INK,
        );
    }
}

/// Minimalist cut-paper Rem nightcap medallion (orthographic flat paper shapes + drop shadow).
pub fn draw_rem_medallion(ui: &mut Ui, size: f32) {
    let _ = draw_rem_medallion_with_sense(ui, size, Sense::hover());
}

/// Interactive variant of the Rem nightcap medallion returning its click/hover `Response`.
pub fn draw_rem_medallion_interactive(ui: &mut Ui, size: f32) -> egui::Response {
    draw_rem_medallion_with_sense(ui, size, Sense::click())
}

fn draw_rem_medallion_with_sense(ui: &mut Ui, size: f32, sense: Sense) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), sense);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.48;

    // Medallion shadow + deep sapphire paper disc
    p.circle_filled(c + Vec2::new(1.5, 2.5), r, Color32::from_black_alpha(115));
    p.circle_filled(c, r, Palette::BLUE_SUBTLE);
    p.circle_stroke(c, r, Stroke::new(1.0_f32, Palette::BLUE_BORDER));

    // Pointed bat-cat ears (flat cut triangles)
    let fur = Color32::from_rgb(16, 17, 22);
    p.add(egui::Shape::convex_polygon(
        vec![
            c + Vec2::new(-r * 0.52, -r * 0.05),
            c + Vec2::new(-r * 0.68, -r * 0.58),
            c + Vec2::new(-r * 0.16, -r * 0.28),
        ],
        fur,
        Stroke::NONE,
    ));
    p.add(egui::Shape::convex_polygon(
        vec![
            c + Vec2::new(r * 0.16, -r * 0.28),
            c + Vec2::new(r * 0.68, -r * 0.58),
            c + Vec2::new(r * 0.52, -r * 0.05),
        ],
        fur,
        Stroke::NONE,
    ));

    // Sleepy sapphire nightcap triangle + tiny brass tassel dot
    p.add(egui::Shape::convex_polygon(
        vec![
            c + Vec2::new(-r * 0.36, -r * 0.24),
            c + Vec2::new(r * 0.08, -r * 0.78),
            c + Vec2::new(r * 0.42, -r * 0.20),
        ],
        Palette::BLUE_HOVER,
        Stroke::NONE,
    ));
    p.circle_filled(
        c + Vec2::new(r * 0.14, -r * 0.76),
        (size * 0.055).max(1.8),
        Palette::WARN_TEXT,
    );

    // Head circle + two glowing pale-ice eyes
    p.circle_filled(c + Vec2::new(0.0, r * 0.12), r * 0.48, fur);
    let eye_col = Color32::from_rgb(186, 222, 255);
    p.circle_filled(
        c + Vec2::new(-r * 0.18, r * 0.10),
        (size * 0.085).max(2.2),
        eye_col,
    );
    p.circle_filled(
        c + Vec2::new(r * 0.18, r * 0.10),
        (size * 0.085).max(2.2),
        eye_col,
    );
    resp
}

fn help_tip_card_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(18, 20, 26))
        .stroke(Stroke::new(1.0_f32, Palette::BLUE_BORDER))
        .corner_radius(CornerRadius::same(6))
        .shadow(egui::Shadow {
            offset: [2, 5],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(175),
        })
        .inner_margin(Margin::symmetric(12, 10))
}

fn render_help_tip_card(ui: &mut Ui, title: &str, body: &str, pinned: bool) {
    ui.set_max_width(296.0);
    ui.spacing_mut().item_spacing.y = 5.0;
    ui.style_mut().interaction.selectable_labels = pinned;

    if !title.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .font(semibold_font(12.5))
                    .color(Palette::TEXT_PRIMARY),
            )
            .wrap(),
        );
    }
    if !body.is_empty() {
        ui.add(
            egui::Label::new(
                RichText::new(body)
                    .size(12.0)
                    .color(Palette::TEXT_SECONDARY),
            )
            .wrap(),
        );
    }
    if pinned {
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new("Click outside or press Esc to close")
                    .size(10.5)
                    .color(Palette::TEXT_MUTED),
            )
            .selectable(false),
        );
    }
}

/// Compact circular `?` help-tip icon button (`18×18 px`).
///
/// - Hovering shows a dark obsidian floating info card with `title` and wrapped `body`.
/// - Clicking toggles a pinned floating popup (with selectable text) dismissed by clicking
///   outside, pressing `Esc`, or clicking `?` again.
pub fn help_tip_icon(ui: &mut egui::Ui, id_salt: &str, title: &str, body: &str) {
    let popup_id = ui.make_persistent_id(id_salt);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());

    let just_toggled = resp.clicked();
    if just_toggled {
        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
    }
    let is_open = ui.memory(|mem| mem.is_popup_open(popup_id));

    if resp.hovered() && ui.is_enabled() {
        ui.ctx().set_cursor_icon(CursorIcon::Help);
    }

    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let (bg, border, fg) = if !enabled {
            (
                Palette::BG_PANEL,
                Palette::BORDER_SUBTLE,
                Palette::TEXT_MUTED,
            )
        } else if is_open {
            (
                Palette::BLUE_PRIMARY,
                Palette::BLUE_LIGHT,
                Palette::TEXT_PRIMARY,
            )
        } else if resp.hovered() {
            (
                Palette::BLUE_SUBTLE,
                Palette::BLUE_LIGHT,
                Palette::BLUE_LIGHT,
            )
        } else {
            (
                Palette::BG_ELEVATED,
                Palette::BORDER_DEFAULT,
                Palette::TEXT_SECONDARY,
            )
        };

        let painter = ui.painter();
        let center = rect.center();
        let radius = rect.width().min(rect.height()) * 0.5 - 0.5;
        painter.circle_filled(
            center + Vec2::new(0.0, 1.0),
            radius,
            Color32::from_black_alpha(80),
        );
        painter.circle_filled(center, radius, bg);
        painter.circle_stroke(center, radius, Stroke::new(1.0_f32, border));

        let galley = painter.layout_no_wrap("?".to_string(), semibold_font(11.0), fg);
        painter.galley(
            Pos2::new(
                center.x - galley.size().x * 0.5,
                center.y - galley.size().y * 0.5,
            ),
            galley,
            fg,
        );
    }

    if !is_open && !(resp.hovered() && ui.is_enabled()) {
        return;
    }

    let mut anchor_rect = rect;
    if let Some(to_global) = ui.ctx().layer_transform_to_global(ui.layer_id()) {
        anchor_rect = to_global * anchor_rect;
    }

    let card_max_w = 296.0_f32;
    let screen = ui.ctx().screen_rect();
    let safe_screen = if screen.width() > 24.0 && screen.height() > 24.0 {
        screen.shrink(8.0)
    } else {
        screen
    };
    let open_left = anchor_rect.left() + card_max_w + 32.0 > screen.right()
        && anchor_rect.right() - (card_max_w + 32.0) >= screen.left();
    let open_above = anchor_rect.bottom() + 140.0 > screen.bottom()
        && anchor_rect.top() - 140.0 >= screen.top();
    let (pivot, anchor_pos) = match (open_left, open_above) {
        (false, false) => (
            egui::Align2::LEFT_TOP,
            Pos2::new(anchor_rect.left(), anchor_rect.bottom() + 6.0),
        ),
        (true, false) => (
            egui::Align2::RIGHT_TOP,
            Pos2::new(anchor_rect.right(), anchor_rect.bottom() + 6.0),
        ),
        (false, true) => (
            egui::Align2::LEFT_BOTTOM,
            Pos2::new(anchor_rect.left(), anchor_rect.top() - 6.0),
        ),
        (true, true) => (
            egui::Align2::RIGHT_BOTTOM,
            Pos2::new(anchor_rect.right(), anchor_rect.top() - 6.0),
        ),
    };

    if is_open {
        let area_resp = egui::Area::new(popup_id.with("pinned_card"))
            .kind(egui::UiKind::Popup)
            .order(egui::Order::Tooltip)
            .fixed_pos(anchor_pos)
            .pivot(pivot)
            .constrain_to(safe_screen)
            .default_width(card_max_w)
            .show(ui.ctx(), |ui| {
                help_tip_card_frame().show(ui, |ui| {
                    render_help_tip_card(ui, title, body, true);
                });
            });

        let esc_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let clicked_outside =
            !just_toggled && resp.clicked_elsewhere() && area_resp.response.clicked_elsewhere();
        if esc_pressed || clicked_outside {
            ui.memory_mut(|mem| mem.close_popup());
        }
    } else {
        egui::Area::new(popup_id.with("hover_card"))
            .kind(egui::UiKind::Popup)
            .order(egui::Order::Tooltip)
            .interactable(false)
            .sense(Sense::hover())
            .fixed_pos(anchor_pos)
            .pivot(pivot)
            .constrain_to(safe_screen)
            .default_width(card_max_w)
            .show(ui.ctx(), |ui| {
                help_tip_card_frame().show(ui, |ui| {
                    render_help_tip_card(ui, title, body, false);
                });
            });
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "—".into();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= 1024.0 && idx < units.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.2} {}", val, units[idx])
    }
}

/// Lightweight syntax highlighter for Steam `stplug-in` `.lua` manifests.
pub fn lua_layouter(ui: &Ui, text: &str, wrap_width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let mono = FontId::new(12.5, FontFamily::Monospace);

    let fmt_plain = TextFormat {
        font_id: mono.clone(),
        color: Palette::TEXT_PRIMARY,
        ..Default::default()
    };
    let fmt_comment = TextFormat {
        font_id: mono.clone(),
        color: Palette::TEXT_MUTED,
        ..Default::default()
    };
    let fmt_fn = TextFormat {
        font_id: mono.clone(),
        color: Palette::BLUE_LIGHT,
        ..Default::default()
    };
    let fmt_str = TextFormat {
        font_id: mono.clone(),
        color: Palette::GO_TEXT,
        ..Default::default()
    };
    let fmt_num = TextFormat {
        font_id: mono,
        color: Palette::TEXT_SECONDARY,
        ..Default::default()
    };

    for (line_idx, line) in text.split('\n').enumerate() {
        if line_idx > 0 {
            job.append("\n", 0.0, fmt_plain.clone());
        }
        let (code_part, comment_part) = match line.find("--") {
            Some(pos) => (&line[..pos], Some(&line[pos..])),
            None => (line, None),
        };

        let mut chars = code_part.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if ch == '"' || ch == '\'' {
                let quote = ch;
                let start = i;
                let mut end = code_part.len();
                for (j, c2) in chars.by_ref() {
                    if c2 == quote {
                        end = j + c2.len_utf8();
                        break;
                    }
                }
                job.append(&code_part[start..end], 0.0, fmt_str.clone());
            } else if ch.is_ascii_alphabetic() || ch == '_' {
                let start = i;
                let mut end = start + ch.len_utf8();
                while let Some(&(j, c2)) = chars.peek() {
                    if c2.is_ascii_alphanumeric() || c2 == '_' {
                        end = j + c2.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                let word = &code_part[start..end];
                if matches!(word, "addappid" | "setManifestid" | "addtoken") {
                    job.append(word, 0.0, fmt_fn.clone());
                } else {
                    job.append(word, 0.0, fmt_plain.clone());
                }
            } else if ch.is_ascii_digit() {
                let start = i;
                let mut end = start + ch.len_utf8();
                while let Some(&(j, c2)) = chars.peek() {
                    if c2.is_ascii_digit() {
                        end = j + c2.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                job.append(&code_part[start..end], 0.0, fmt_num.clone());
            } else {
                job.append(&code_part[i..i + ch.len_utf8()], 0.0, fmt_plain.clone());
            }
        }

        if let Some(cmt) = comment_part {
            job.append(cmt, 0.0, fmt_comment.clone());
        }
    }

    ui.fonts(|f| f.layout_job(job))
}

// ===== Native Linux File / Folder Pickers (zenity / kdialog) =====

pub fn pick_files_native(title: &str, filter_glob: Option<&str>) -> Vec<PathBuf> {
    let mut cmd = Command::new("zenity");
    cmd.args([
        "--file-selection",
        "--multiple",
        "--separator=\n",
        "--title",
        title,
    ]);
    if let Some(f) = filter_glob {
        cmd.arg(format!("--file-filter={}", f));
    }
    if let Ok(out) = cmd.output() {
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

    let mut kcmd = Command::new("kdialog");
    kcmd.args([
        "--getopenfilename",
        ".",
        "--multiple",
        "--separate-output",
        "--title",
        title,
    ]);
    if let Ok(out) = kcmd.output() {
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
        .args(["--file-selection", "--directory", "--title", title])
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
        .args(["--getexistingdirectory", ".", "--title", title])
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
