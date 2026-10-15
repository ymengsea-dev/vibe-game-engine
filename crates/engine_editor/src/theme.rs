//! Studio look-and-feel: the base text sizes / spacing applied on top of
//! egui's dark theme.
//!
//! Google Sans Flex Text SemiBold is bundled for proportional UI text.
//! It is compiled into the binary under the SIL Open Font License 1.1;
//! see `crates/engine_editor/fonts/OFL.txt`. Egui's bundled Hack face
//! remains the monospace font. Every platform therefore renders
//! identically and the studio reads no system font files.

use egui::{Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

const GOOGLE_SANS_FLEX_SEMIBOLD: &str = "Google Sans Flex Text SemiBold";

/// Installs the studio fonts and base style into `ctx`. Call once, right
/// after the [`Context`] is created.
pub fn install(ctx: &Context) {
    install_fonts(ctx);
    install_style(ctx);
}

fn install_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        GOOGLE_SANS_FLEX_SEMIBOLD.to_owned(),
        FontData::from_static(include_bytes!("../fonts/GoogleSansFlexText-SemiBold.ttf")).into(),
    );
    if let Some(proportional) = fonts.families.get_mut(&FontFamily::Proportional) {
        proportional.insert(0, GOOGLE_SANS_FLEX_SEMIBOLD.to_owned());
    }
    fonts
}

/// Base text sizes and spacing — a little larger and airier than egui's
/// defaults, to match the density of the UI reference.
fn install_style(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(16.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(7.0, 5.0);
        style.spacing.button_padding = egui::vec2(7.0, 3.0);
        style.spacing.interact_size.y = 26.0;
        style.visuals.panel_fill = egui::Color32::from_rgb(22, 25, 31);
        style.visuals.window_fill = egui::Color32::from_rgb(28, 32, 39);
        style.visuals.faint_bg_color = egui::Color32::from_rgb(33, 38, 47);
        style.visuals.extreme_bg_color = egui::Color32::from_rgb(15, 17, 21);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(42, 102, 176);
        style.visuals.selection.stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 205, 255));
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(45, 52, 64);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(42, 102, 176);
        style.visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(53, 60, 72));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_tweaks_are_applied() {
        let ctx = Context::default();
        install_style(&ctx);
        let style = ctx.style_of(egui::Theme::Dark);
        let body = &style.text_styles[&TextStyle::Body];
        assert_eq!(body.size, 13.5);
        assert_eq!(body.family, FontFamily::Proportional);
        assert_eq!(style.spacing.item_spacing, egui::vec2(7.0, 5.0));
    }

    #[test]
    fn style_uses_minimal_dark_surfaces_and_blue_selection() {
        let ctx = Context::default();
        install_style(&ctx);
        let visuals = &ctx.style_of(egui::Theme::Dark).visuals;
        assert_eq!(visuals.panel_fill, egui::Color32::from_rgb(22, 25, 31));
        assert_eq!(
            visuals.selection.bg_fill,
            egui::Color32::from_rgb(42, 102, 176)
        );
    }

    #[test]
    fn install_is_infallible_and_leaves_egui_default_families_intact() {
        let ctx = Context::default();
        install(&ctx);
        // egui always seeds both families with at least its own faces.
        let fonts = ctx.style_of(egui::Theme::Dark);
        assert_eq!(
            fonts.text_styles[&TextStyle::Monospace].family,
            FontFamily::Monospace
        );
    }

    #[test]
    fn proportional_fonts_prefer_google_sans_flex_semibold() {
        let fonts = font_definitions();
        let proportional = fonts
            .families
            .get(&FontFamily::Proportional)
            .and_then(|family| family.first());

        assert_eq!(
            proportional.map(String::as_str),
            Some(GOOGLE_SANS_FLEX_SEMIBOLD)
        );
        assert!(fonts.font_data.contains_key(GOOGLE_SANS_FLEX_SEMIBOLD));
    }
}
