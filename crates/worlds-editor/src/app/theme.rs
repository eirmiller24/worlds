//! Dark theme, one accent color, spacing on an 8 px grid.

use egui::{Color32, Stroke, Vec2};

/// The one accent color: selection, active level, active tool.
pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x9b, 0xe8);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(8.0, 4.0);
        s.button_padding = Vec2::new(8.0, 4.0);
        s.window_margin = egui::Margin::same(8);
        s.menu_margin = egui::Margin::same(8);
        s.indent = 16.0;
        s.interact_size.y = 24.0;
        let v = &mut style.visuals;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.6);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.window_corner_radius = egui::CornerRadius::same(4);
        v.panel_fill = Color32::from_gray(30);
        v.window_fill = Color32::from_gray(34);
        v.extreme_bg_color = Color32::from_gray(20);
    });
}
