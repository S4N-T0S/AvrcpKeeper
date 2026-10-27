use std::sync::Arc;

use eframe::egui::{
    self, style::ScrollStyle, style::TextCursorStyle, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Shadow, Stroke, TextStyle,
};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);
pub const SURFACE: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x23);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x23, 0x27, 0x2F);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x2B, 0x30, 0x39);
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x2E, 0x37);
pub const BORDER_HI: Color32 = Color32::from_rgb(0x3A, 0x40, 0x4B);
pub const SELECTED: Color32 = Color32::from_rgb(0x3D, 0x44, 0x50);
pub const SELECTED_EDGE: Color32 = Color32::from_rgb(0x4A, 0x52, 0x60);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xEE);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9B, 0xA1, 0xAD);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x7F, 0x85, 0x90);
pub const ACCENT: Color32 = Color32::from_rgb(0x5E, 0x8E, 0xFF);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x7A, 0xA2, 0xFF);
pub const BUTTON: Color32 = Color32::from_rgb(0x3A, 0x6A, 0xE0);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(0x33, 0x60, 0xD6);
pub const BUTTON_PRESSED: Color32 = Color32::from_rgb(0x2D, 0x56, 0xC2);
pub const GOOD: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x4B);
pub const ERROR: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x7A);

pub const SIZE_TITLE: f32 = 20.0;
pub const SIZE_CARD_TITLE: f32 = 15.0;
pub const SIZE_BODY: f32 = 14.0;
pub const SIZE_SECONDARY: f32 = 13.0;
pub const SIZE_CAPTION: f32 = 12.0;

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(SIZE_TITLE, semibold())),
            (TextStyle::Body, FontId::proportional(SIZE_BODY)),
            (TextStyle::Button, FontId::proportional(SIZE_BODY)),
            (TextStyle::Small, FontId::proportional(SIZE_CAPTION)),
            (TextStyle::Monospace, FontId::monospace(SIZE_SECONDARY)),
        ]
        .into();
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.combo_height = 260.0;
        style.spacing.scroll = ScrollStyle::solid();

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = SURFACE_HI;
        v.faint_bg_color = SURFACE;
        v.code_bg_color = SURFACE_HI;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        let shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = shadow;
        v.window_shadow = shadow;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.text_cursor = TextCursorStyle { stroke: Stroke::new(1.5, ACCENT), ..Default::default() };

        let radius = CornerRadius::same(7);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = SURFACE;
        w.noninteractive.weak_bg_fill = SURFACE;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;
        for (state, fill, stroke) in [
            (&mut w.inactive, SURFACE_HI, BORDER),
            (&mut w.hovered, SURFACE_HOVER, BORDER_HI),
            (&mut w.active, SURFACE_HOVER, ACCENT),
            (&mut w.open, SURFACE_HOVER, BORDER_HI),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, stroke);
            state.fg_stroke = Stroke::new(1.0, TEXT);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
    });
}

/// Uses Segoe UI when available; egui's bundled fonts stay as fallback for missing glyphs.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = crate::platform::windows_dir().join("Fonts");
    let mut proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut heavy = proportional.clone();
    if let Ok(bytes) = std::fs::read(dir.join("segoeui.ttf")) {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(bytes)));
        proportional.insert(0, "ui".into());
        heavy.insert(0, "ui".into());
    }
    if let Ok(bytes) = std::fs::read(dir.join("seguisb.ttf")) {
        fonts.font_data.insert("ui-semibold".into(), Arc::new(FontData::from_owned(bytes)));
        heavy.insert(0, "ui-semibold".into());
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(semibold(), heavy);
    ctx.set_fonts(fonts);
}
