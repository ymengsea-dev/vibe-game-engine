//! Studio look-and-feel: the base text sizes / spacing applied on top of
//! egui's dark theme.
//!
//! The approved prototype's regular Roboto is bundled for proportional UI
//! text. Egui's bundled monospace face remains the code/terminal fallback.

use egui::{Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

const ROBOTO: &str = "Roboto";
pub(crate) const SHELL_FILL: egui::Color32 = egui::Color32::from_rgb(11, 17, 26);
pub(crate) const BAR_FILL: egui::Color32 = egui::Color32::from_rgb(17, 26, 38);
pub(crate) const PANEL_FILL: egui::Color32 = egui::Color32::from_rgb(21, 29, 41);
pub(crate) const PANEL_RAISED: egui::Color32 = egui::Color32::from_rgb(27, 37, 51);
pub(crate) const PANEL_HOVER: egui::Color32 = egui::Color32::from_rgb(35, 48, 68);
pub(crate) const BORDER: egui::Color32 = egui::Color32::from_rgb(43, 59, 80);
pub(crate) const BORDER_SOFT: egui::Color32 = egui::Color32::from_rgb(37, 51, 70);
pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(228, 234, 243);
pub(crate) const MUTED: egui::Color32 = egui::Color32::from_rgb(145, 160, 180);
pub(crate) const SUBTLE: egui::Color32 = egui::Color32::from_rgb(100, 116, 139);
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(131, 181, 255);
pub(crate) const ACCENT_FILL: egui::Color32 = egui::Color32::from_rgb(40, 75, 125);

/// Installs the studio fonts and base style into `ctx`. Call once, right
/// after the [`Context`] is created.
pub fn install(ctx: &Context) {
    install_fonts(ctx);
    install_style(ctx);
}

pub(crate) fn modal<R>(
    ctx: &Context,
    id: egui::Id,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    egui::Modal::new(id)
        .frame(
            egui::Frame::NONE
                .fill(PANEL_RAISED)
                .stroke(egui::Stroke::new(1.0, BORDER_SOFT))
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::symmetric(20, 18))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(100),
                }),
        )
        .backdrop_color(egui::Color32::from_black_alpha(150))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
            ui.spacing_mut().interact_size.y = 30.0;
            ui.style_mut().text_styles.insert(
                TextStyle::Heading,
                FontId::new(16.0, FontFamily::Proportional),
            );
            add_contents(ui)
        })
}

pub(crate) fn style_context_menu(ui: &mut egui::Ui) {
    ui.set_min_width(180.0);
    ui.spacing_mut().button_padding = egui::vec2(10.0, 7.0);
    ui.spacing_mut().item_spacing.y = 4.0;
}

fn install_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        ROBOTO.to_owned(),
        FontData::from_static(include_bytes!("../fonts/Roboto-Variable.ttf")).into(),
    );
    if let Some(proportional) = fonts.families.get_mut(&FontFamily::Proportional) {
        proportional.insert(0, ROBOTO.to_owned());
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
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(12.0, FontFamily::Proportional),
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
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 3.0);
        style.spacing.interact_size.y = 24.0;
        style.visuals.panel_fill = SHELL_FILL;
        style.visuals.window_fill = PANEL_RAISED;
        style.visuals.faint_bg_color = PANEL_RAISED;
        style.visuals.extreme_bg_color = SHELL_FILL;
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.selection.bg_fill = ACCENT_FILL;
        style.visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        style.visuals.widgets.noninteractive.fg_stroke.color = MUTED;
        style.visuals.widgets.noninteractive.bg_fill = PANEL_FILL;
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.inactive.bg_fill = PANEL_RAISED;
        style.visuals.widgets.inactive.fg_stroke.color = MUTED;
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(5);
        style.visuals.widgets.hovered.bg_fill = PANEL_HOVER;
        style.visuals.widgets.hovered.fg_stroke.color = TEXT;
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(5);
        style.visuals.widgets.active.bg_fill = ACCENT_FILL;
        style.visuals.widgets.active.fg_stroke.color = TEXT;
        style.visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(5);
        style.visuals.window_stroke = egui::Stroke::NONE;
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
        assert_eq!(body.size, 12.0);
        assert_eq!(body.family, FontFamily::Proportional);
        assert_eq!(
            style.text_styles[&TextStyle::Heading].family,
            FontFamily::Proportional
        );
        assert_eq!(style.spacing.item_spacing, egui::vec2(6.0, 4.0));
        assert_eq!(style.spacing.interact_size.y, 24.0);
    }

    #[test]
    fn style_matches_prototype_dark_surfaces_and_blue_selection() {
        let ctx = Context::default();
        install_style(&ctx);
        let visuals = &ctx.style_of(egui::Theme::Dark).visuals;
        assert_eq!(visuals.panel_fill, egui::Color32::from_rgb(11, 17, 26));
        assert_eq!(
            visuals.extreme_bg_color,
            egui::Color32::from_rgb(11, 17, 26)
        );
        assert_eq!(
            visuals.selection.bg_fill,
            egui::Color32::from_rgb(40, 75, 125)
        );
    }

    #[test]
    fn panel_boundaries_use_surface_contrast_not_strokes() {
        let ctx = Context::default();
        install_style(&ctx);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.window_stroke.width,
            0.0
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Dark)
                .visuals
                .widgets
                .noninteractive
                .bg_stroke
                .width,
            0.0
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Dark)
                .visuals
                .widgets
                .hovered
                .bg_stroke
                .width,
            0.0
        );
        assert_eq!(
            ctx.style_of(egui::Theme::Dark)
                .visuals
                .widgets
                .active
                .bg_stroke
                .width,
            0.0
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
    fn editor_fonts_prefer_regular_roboto_with_default_monospace_fallback() {
        let fonts = font_definitions();
        let proportional = &fonts.families[&FontFamily::Proportional];
        let monospace = &fonts.families[&FontFamily::Monospace];
        assert_eq!(proportional.first().map(String::as_str), Some(ROBOTO));
        assert!(fonts.font_data.contains_key(ROBOTO));
        assert!(monospace.len() > 1);
        assert!(
            ab_glyph::FontRef::try_from_slice(&include_bytes!("../fonts/Roboto-Variable.ttf")[..])
                .is_ok()
        );
    }

    #[test]
    fn approved_prototype_uses_regular_roboto_and_blue_dark_surfaces() {
        let fonts = font_definitions();
        assert_eq!(fonts.families[&FontFamily::Proportional][0], "Roboto");

        let ctx = Context::default();
        install_style(&ctx);
        let style = ctx.style_of(egui::Theme::Dark);
        assert_eq!(style.text_styles[&TextStyle::Body].size, 12.0);
        assert_eq!(
            style.visuals.panel_fill,
            egui::Color32::from_rgb(11, 17, 26)
        );
        assert_eq!(
            style.visuals.extreme_bg_color,
            egui::Color32::from_rgb(11, 17, 26)
        );
        assert_eq!(
            style.visuals.selection.bg_fill,
            egui::Color32::from_rgb(40, 75, 125)
        );
    }

    #[test]
    fn workspace_gaps_show_the_shell_surface_not_the_card_surface() {
        let ctx = Context::default();
        install_style(&ctx);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            SHELL_FILL
        );
    }
}
