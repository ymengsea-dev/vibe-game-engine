//! Studio look-and-feel: the base text sizes / spacing applied on top of
//! egui's dark theme.
//!
//! Fonts are **egui's own bundled faces** — Ubuntu-Light (proportional)
//! and Hack (monospace), both open-licensed and compiled into the
//! binary. Every platform therefore renders identically and the studio
//! reads no system font files (it previously loaded Apple's SF Pro / SF
//! Mono from `/System/Library/Fonts`, which only worked on macOS).
//!
//! Swapping in a house face (e.g. Inter) is a drop-in change once the
//! `.ttf` is vendored under `crates/engine_editor/fonts/`: add an
//! `include_bytes!` face in [`install_fonts`] and prepend it to the
//! proportional family.

use egui::{Context, FontFamily, FontId, TextStyle};

/// Installs the studio fonts and base style into `ctx`. Call once, right
/// after the [`Context`] is created.
pub fn install(ctx: &Context) {
    install_fonts(ctx);
    install_style(ctx);
}

/// Currently a no-op — egui's bundled cross-platform faces are kept as
/// is. Left as a seam for vendoring a house face later (see the module
/// docs).
fn install_fonts(_ctx: &Context) {}

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
}
