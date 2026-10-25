//! Terminal look: pixel font (Monocraft), green-on-black, square corners,
//! hard 1–2px borders and no shadows.

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke, TextStyle};

pub const BG: Color32 = Color32::from_rgb(10, 14, 10);
pub const BG_DEEP: Color32 = Color32::from_rgb(4, 7, 4);
pub const BG_HOVER: Color32 = Color32::from_rgb(20, 44, 24);
pub const FG: Color32 = Color32::from_rgb(125, 255, 143);
pub const DIM: Color32 = Color32::from_rgb(60, 122, 70);
pub const AMBER: Color32 = Color32::from_rgb(255, 204, 51);
pub const RED: Color32 = Color32::from_rgb(255, 95, 86);

/// Stroke of `w` pixels.
pub fn px(w: f32, c: Color32) -> Stroke {
    Stroke::new(w, c)
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "monocraft".into(),
        FontData::from_static(include_bytes!("../assets/Monocraft.ttf")),
    );
    // Monocraft first; egui's bundled fonts stay as fallback for glyphs it lacks.
    for fam in [FontFamily::Monospace, FontFamily::Proportional] {
        fonts.families.entry(fam).or_default().insert(0, "monocraft".into());
    }
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    let mono = |size| FontId::new(size, FontFamily::Monospace);
    style.text_styles = [
        (TextStyle::Heading, mono(20.0)),
        (TextStyle::Body, mono(14.0)),
        (TextStyle::Button, mono(14.0)),
        (TextStyle::Monospace, mono(14.0)),
        (TextStyle::Small, mono(11.0)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = BG_DEEP;
    v.hyperlink_color = AMBER;
    v.error_fg_color = RED;
    v.warn_fg_color = AMBER;
    v.window_rounding = Rounding::ZERO;
    v.menu_rounding = Rounding::ZERO;
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_stroke = px(2.0, FG);
    v.selection.bg_fill = FG;
    v.selection.stroke = px(1.0, BG);

    let w = &mut v.widgets;
    for (state, fill, stroke, text) in [
        (&mut w.noninteractive, BG, px(1.0, DIM), FG),
        (&mut w.inactive, BG, px(1.0, DIM), FG),
        (&mut w.hovered, BG_HOVER, px(2.0, FG), FG),
        (&mut w.active, FG, px(2.0, FG), BG),
        (&mut w.open, BG_HOVER, px(2.0, AMBER), AMBER),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = stroke;
        state.fg_stroke = px(1.0, text);
        state.rounding = Rounding::ZERO;
        state.expansion = 0.0;
    }
    ctx.set_style(style);
}

/// A titled box with a hard border, like a TUI pane.
pub fn pane<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::none()
        .stroke(px(1.0, DIM))
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(egui::RichText::new(format!("[ {title} ]")).color(AMBER));
            }
            add(ui)
        })
        .inner
}

/// `[#########-----------]` with a fixed width so rows line up.
pub fn ascii_bar(frac: f32, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f32).round() as usize;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
}

#[cfg(test)]
mod tests {
    #[test]
    fn bar_widths() {
        assert_eq!(super::ascii_bar(0.0, 4), "[----]");
        assert_eq!(super::ascii_bar(0.5, 4), "[##--]");
        assert_eq!(super::ascii_bar(2.0, 4), "[####]");
    }
}
