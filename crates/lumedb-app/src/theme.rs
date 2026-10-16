//! Look in the style of classic desktop tools, in light and dark.

use eframe::egui::{self, Color32, FontId, Rounding, Stroke, TextStyle};
use lumedb_core::DbKind;

pub const MUTED: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);

// Icon colors.
pub const BLUE: Color32 = Color32::from_rgb(0x1E, 0x78, 0xD2);
pub const ORANGE: Color32 = Color32::from_rgb(0xE8, 0x8B, 0x1A);
pub const GREEN: Color32 = Color32::from_rgb(0x3C, 0xA0, 0x4A);
pub const PURPLE: Color32 = Color32::from_rgb(0x8E, 0x5C, 0xC8);
pub const TEAL: Color32 = Color32::from_rgb(0x14, 0x9C, 0xA0);
pub const RED: Color32 = Color32::from_rgb(0xD2, 0x4B, 0x3C);

// Edit states in the grid.
pub const EDITED: Color32 = Color32::from_rgba_premultiplied(0x60, 0x50, 0x00, 0x30);
pub const INSERTED: Color32 = Color32::from_rgba_premultiplied(0x10, 0x50, 0x10, 0x30);
pub const DELETED: Color32 = Color32::from_rgba_premultiplied(0x60, 0x10, 0x10, 0x30);

fn dark(ctx: &egui::Context) -> bool {
    ctx.style().visuals.dark_mode
}

macro_rules! pick {
    ($name:ident, $light:expr, $dark:expr) => {
        pub fn $name(ctx: &egui::Context) -> Color32 {
            if dark(ctx) {
                $dark
            } else {
                $light
            }
        }
    };
}

pick!(bg, Color32::WHITE, Color32::from_rgb(0x1E, 0x1F, 0x22));
pick!(panel, Color32::from_rgb(0xF3, 0xF3, 0xF3), Color32::from_rgb(0x2B, 0x2D, 0x30));
pick!(ribbon, Color32::from_rgb(0xFA, 0xFA, 0xFA), Color32::from_rgb(0x2B, 0x2D, 0x30));
pick!(border, Color32::from_rgb(0xD6, 0xD6, 0xD6), Color32::from_rgb(0x43, 0x45, 0x4A));
pick!(grid, Color32::from_rgb(0xE2, 0xE2, 0xE2), Color32::from_rgb(0x3A, 0x3C, 0x40));
pick!(hover, Color32::from_rgb(0xE3, 0xEA, 0xF3), Color32::from_rgb(0x35, 0x3B, 0x45));
pick!(tab_inactive, Color32::from_rgb(0xE6, 0xE6, 0xE6), Color32::from_rgb(0x25, 0x27, 0x2A));
pick!(text, Color32::from_rgb(0x20, 0x20, 0x20), Color32::from_rgb(0xDF, 0xE1, 0xE5));
pick!(row_selected, Color32::from_rgb(0xDC, 0xEB, 0xFB), Color32::from_rgb(0x26, 0x38, 0x50));
pick!(header, Color32::from_rgb(0xF3, 0xF3, 0xF3), Color32::from_rgb(0x2B, 0x2D, 0x30));

pub fn kind_color(kind: DbKind) -> Color32 {
    match kind {
        DbKind::Mysql => Color32::from_rgb(0x00, 0x75, 0x8F),
        DbKind::Postgres => Color32::from_rgb(0x33, 0x67, 0x91),
        DbKind::Sqlite => Color32::from_rgb(0x0F, 0x80, 0xCC),
    }
}

pub fn apply(ctx: &egui::Context, dark_mode: bool) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut v = if dark_mode { egui::Visuals::dark() } else { egui::Visuals::light() };
    if dark_mode {
        v.panel_fill = Color32::from_rgb(0x2B, 0x2D, 0x30);
        v.window_fill = Color32::from_rgb(0x2B, 0x2D, 0x30);
        v.extreme_bg_color = Color32::from_rgb(0x1E, 0x1F, 0x22);
        v.faint_bg_color = Color32::from_rgb(0x25, 0x27, 0x2A);
        v.selection.bg_fill = Color32::from_rgb(0x2E, 0x43, 0x6E);
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x35, 0x3B, 0x45);
    } else {
        v.panel_fill = Color32::from_rgb(0xF3, 0xF3, 0xF3);
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(0xF5, 0xF8, 0xFC);
        v.selection.bg_fill = Color32::from_rgb(0xCC, 0xE4, 0xF7);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0x20, 0x20, 0x20));
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xE3, 0xEA, 0xF3);
        v.widgets.hovered.bg_fill = Color32::from_rgb(0xE3, 0xEA, 0xF3);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0xD6, 0xD6, 0xD6));
    }
    // Thin border on text boxes and buttons (otherwise they are invisible on a white background).
    let edge = if dark_mode { Color32::from_rgb(0x4A, 0x4D, 0x52) } else { Color32::from_rgb(0xC8, 0xC8, 0xC8) };
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, edge);
    v.window_rounding = Rounding::same(6.0);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = Rounding::same(3.0);
    }
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 3.0);
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(13.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
}
