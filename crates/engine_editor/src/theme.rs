//! Studio look-and-feel: one palette ([`NIGHT`], Tokyo Night colours with
//! soft, Apple-like surfaces), the fonts (San Francisco and Menlo from
//! macOS, the bundled Roboto elsewhere), and the base text sizes and
//! spacing. The rules for using them are in `docs/dev/04_UI_STYLE.md`.

use egui::{Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

const ROBOTO: &str = "Roboto";

/// A complete set of Studio colours. Panels draw with one palette; the
/// standard egui widgets inside them follow it through
/// [`Palette::apply`], and Studio's own drawn controls take it as an
/// argument. Studio has one palette, [`NIGHT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Palette {
    /// Behind panels, and the track of segmented controls.
    pub(crate) shell: egui::Color32,
    /// Panel background.
    pub(crate) panel: egui::Color32,
    /// Raised surfaces: cards, windows, pop-ups.
    pub(crate) raised: egui::Color32,
    /// A hovered control.
    pub(crate) hover: egui::Color32,
    /// The selected segment of a segmented control.
    pub(crate) selected: egui::Color32,
    /// Panel and window edges (kept faint).
    pub(crate) border: egui::Color32,
    /// Hairline dividers.
    pub(crate) rule: egui::Color32,
    /// Body text.
    pub(crate) text: egui::Color32,
    /// Secondary text and idle icons.
    pub(crate) muted: egui::Color32,
    /// Hints and disabled text.
    pub(crate) subtle: egui::Color32,
    /// Links, focus, toggles, the selected sidebar item.
    pub(crate) accent: egui::Color32,
    /// Behind selected rows and text.
    pub(crate) accent_fill: egui::Color32,
    /// Stop, errors, destructive actions.
    pub(crate) danger: egui::Color32,
    /// Success and "connected".
    pub(crate) success: egui::Color32,
    /// Information and debug messages.
    pub(crate) info: egui::Color32,
    /// A second categorical colour (alongside accent and info).
    pub(crate) purple: egui::Color32,
    /// The selection and the dragged handle inside the 3D view: a warm
    /// colour that stands out against sky and grass.
    pub(crate) highlight: egui::Color32,
}

/// Tokyo Night (the VS Code theme's "Night" colours), used with soft,
/// low-contrast surfaces in the manner of Apple's dark mode.
pub(crate) const NIGHT: Palette = Palette {
    shell: egui::Color32::from_rgb(0x16, 0x16, 0x1E),
    panel: egui::Color32::from_rgb(0x1A, 0x1B, 0x26),
    raised: egui::Color32::from_rgb(0x1F, 0x23, 0x35),
    hover: egui::Color32::from_rgb(0x24, 0x28, 0x3B),
    selected: egui::Color32::from_rgb(0x2F, 0x34, 0x4D),
    border: egui::Color32::from_rgb(0x23, 0x25, 0x37),
    rule: egui::Color32::from_rgb(0x26, 0x29, 0x3D),
    text: egui::Color32::from_rgb(0xC0, 0xCA, 0xF5),
    muted: egui::Color32::from_rgb(0x82, 0x89, 0xAF),
    subtle: egui::Color32::from_rgb(0x56, 0x5F, 0x89),
    accent: egui::Color32::from_rgb(0x7A, 0xA2, 0xF7),
    accent_fill: egui::Color32::from_rgb(0x28, 0x34, 0x57),
    danger: egui::Color32::from_rgb(0xF7, 0x76, 0x8E),
    success: egui::Color32::from_rgb(0x9E, 0xCE, 0x6A),
    info: egui::Color32::from_rgb(0x7D, 0xCF, 0xFF),
    purple: egui::Color32::from_rgb(0xBB, 0x9A, 0xF7),
    highlight: egui::Color32::from_rgb(0xE0, 0xAF, 0x68),
};

impl Palette {
    /// Recolours `visuals` so egui's own widgets (labels, buttons,
    /// checkboxes, drag values, menus) follow this palette: soft filled
    /// surfaces, faint edges, rounded corners.
    pub(crate) fn apply(&self, visuals: &mut egui::Visuals) {
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.raised;
        visuals.window_stroke = egui::Stroke::new(1.0, self.border);
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.menu_corner_radius = egui::CornerRadius::same(10);
        visuals.extreme_bg_color = self.shell;
        visuals.faint_bg_color = self.raised;
        visuals.code_bg_color = self.shell;
        visuals.hyperlink_color = self.accent;
        visuals.warn_fg_color = self.highlight;
        visuals.error_fg_color = self.danger;
        visuals.selection.bg_fill = self.accent_fill;
        visuals.selection.stroke = egui::Stroke::new(1.0, self.accent);
        visuals.override_text_color = None;
        // Soft but square enough that an 11-point checkbox box stays a box
        // (egui draws it with this radius). Studio's own pills and cards set
        // larger radii explicitly.
        let rounded = egui::CornerRadius::same(4);
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = self.panel;
        widgets.noninteractive.weak_bg_fill = self.panel;
        widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, self.rule);
        widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, self.text);
        widgets.noninteractive.corner_radius = rounded;
        widgets.inactive.bg_fill = self.hover;
        widgets.inactive.weak_bg_fill = self.hover;
        widgets.inactive.bg_stroke = egui::Stroke::NONE;
        widgets.inactive.fg_stroke = egui::Stroke::new(1.0, self.text);
        widgets.inactive.corner_radius = rounded;
        widgets.hovered.bg_fill = self.selected;
        widgets.hovered.weak_bg_fill = self.selected;
        widgets.hovered.bg_stroke = egui::Stroke::NONE;
        widgets.hovered.fg_stroke = egui::Stroke::new(1.0, self.text);
        widgets.hovered.corner_radius = rounded;
        widgets.hovered.expansion = 0.0;
        widgets.active.bg_fill = self.accent_fill;
        widgets.active.weak_bg_fill = self.accent_fill;
        widgets.active.bg_stroke = egui::Stroke::NONE;
        widgets.active.fg_stroke = egui::Stroke::new(1.0, self.text);
        widgets.active.corner_radius = rounded;
        widgets.active.expansion = 0.0;
        widgets.open = widgets.hovered;
    }
}

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
                .fill(NIGHT.raised)
                .stroke(egui::Stroke::new(1.0, NIGHT.selected))
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

/// Regular weight on a variable font's `wght` axis: body text.
pub(crate) const REGULAR: f32 = 400.0;
/// Semibold: page titles and section headings, as on macOS.
pub(crate) const SEMIBOLD: f32 = 600.0;
/// The weight of Studio's UI text.
pub(crate) const UI_FONT_WEIGHT: f32 = REGULAR;

/// The macOS system UI font (San Francisco), which VS Code also uses for
/// its interface.
const SF_PRO: &str = "SF Pro";
/// VS Code's default code font on macOS.
const MENLO: &str = "Menlo";
/// Where macOS keeps them.
const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";
/// The optical size San Francisco is drawn at: its "Text" design, which
/// macOS uses for UI text.
const SF_TEXT_OPTICAL_SIZE: f32 = 13.0;
const SYSTEM_MONO_FONT: &str = "/System/Library/Fonts/Menlo.ttc";

/// Fonts read from the operating system rather than bundled: Apple's
/// licence lets an app use the installed San Francisco and Menlo, not ship
/// them. `None` where they are missing (other platforms), and Studio uses
/// its bundled Roboto instead.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemFonts {
    /// San Francisco (a variable font: every weight).
    pub(crate) ui: Option<&'static [u8]>,
    /// Menlo.
    pub(crate) mono: Option<&'static [u8]>,
}

impl SystemFonts {
    /// The system fonts, read once per run and kept for its lifetime.
    pub(crate) fn installed() -> Self {
        static FONTS: std::sync::OnceLock<SystemFonts> = std::sync::OnceLock::new();
        *FONTS.get_or_init(|| {
            let read = |path: &str| {
                if !cfg!(target_os = "macos") {
                    return None;
                }
                match std::fs::read(path) {
                    // Read once and kept for the whole run, so the
                    // egui font table can borrow it without copies.
                    Ok(bytes) => Some(&*Box::leak(bytes.into_boxed_slice())),
                    Err(err) => {
                        tracing::info!(path, error = %err, "system font unavailable; using Roboto");
                        None
                    }
                }
            };
            SystemFonts {
                ui: read(SYSTEM_UI_FONT),
                mono: read(SYSTEM_MONO_FONT),
            }
        })
    }
}

/// Text in the semibold weight, for titles and headings.
pub(crate) fn semibold(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).variation(b"wght", SEMIBOLD)
}

/// A one-line galley in `weight`, for text painted directly; cut with
/// "…" at `max_width` when given.
pub(crate) fn weighted_galley(
    painter: &egui::Painter,
    text: impl Into<String>,
    font_id: FontId,
    color: egui::Color32,
    weight: f32,
    max_width: Option<f32>,
) -> std::sync::Arc<egui::Galley> {
    let format = egui::TextFormat {
        font_id,
        color,
        coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]),
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::single_section(text.into(), format);
    if let Some(max_width) = max_width {
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    }
    painter.layout_job(job)
}

fn install_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

fn font_definitions() -> FontDefinitions {
    font_definitions_with_weight(UI_FONT_WEIGHT)
}

/// The Studio fonts with the UI text at `weight`.
pub(crate) fn font_definitions_with_weight(weight: f32) -> FontDefinitions {
    font_definitions_with(SystemFonts::installed(), weight)
}

/// The Studio fonts: San Francisco for the UI and Menlo for code where the
/// system has them (as VS Code on macOS), with the bundled Roboto and
/// egui's defaults behind them for anything they lack. UI text is set at
/// `weight`; both UI fonts are variable, so every weight comes from one file.
pub(crate) fn font_definitions_with(system: SystemFonts, weight: f32) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let tweak = egui::epaint::text::FontTweak {
        coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]),
        ..Default::default()
    };
    fonts.font_data.insert(
        ROBOTO.to_owned(),
        FontData::from_static(include_bytes!("../fonts/Roboto-Variable.ttf"))
            .tweak(tweak.clone())
            .into(),
    );
    let mut ui_fonts = vec![ROBOTO.to_owned()];
    if let Some(bytes) = system.ui {
        // San Francisco's text optical size: the looser spacing macOS uses
        // for UI-sized text (its default is tuned for large display text).
        let mut sf_tweak = tweak;
        sf_tweak.coords.push(b"opsz", SF_TEXT_OPTICAL_SIZE);
        fonts.font_data.insert(
            SF_PRO.to_owned(),
            FontData::from_static(bytes).tweak(sf_tweak).into(),
        );
        ui_fonts.insert(0, SF_PRO.to_owned());
    }
    if let Some(proportional) = fonts.families.get_mut(&FontFamily::Proportional) {
        for (index, name) in ui_fonts.into_iter().enumerate() {
            proportional.insert(index, name);
        }
    }
    if let Some(bytes) = system.mono {
        fonts
            .font_data
            .insert(MENLO.to_owned(), FontData::from_static(bytes).into());
        if let Some(monospace) = fonts.families.get_mut(&FontFamily::Monospace) {
            monospace.insert(0, MENLO.to_owned());
        }
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
        // egui fades a scroll area's edges in a colour it guesses from the
        // frames around it, which shows as a lighter band on Studio's cards.
        style.spacing.scroll.fade.strength = 0.0;
        NIGHT.apply(&mut style.visuals);
        // Between and behind panels: the darkest surface, so each panel
        // (drawn in `NIGHT.panel`) reads as a soft card on it.
        style.visuals.panel_fill = NIGHT.shell;
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
    fn without_system_fonts_studio_uses_roboto_and_egui_monospace() {
        let fonts = font_definitions_with(SystemFonts::default(), REGULAR);
        let proportional = &fonts.families[&FontFamily::Proportional];
        let monospace = &fonts.families[&FontFamily::Monospace];
        assert_eq!(proportional.first().map(String::as_str), Some(ROBOTO));
        assert!(fonts.font_data.contains_key(ROBOTO));
        assert!(!fonts.font_data.contains_key(SF_PRO));
        assert!(monospace.len() > 1);
        assert!(
            ab_glyph::FontRef::try_from_slice(&include_bytes!("../fonts/Roboto-Variable.ttf")[..])
                .is_ok()
        );
    }

    #[test]
    fn system_fonts_come_first_with_roboto_behind_them() {
        // Any valid font stands in for the system files here.
        let stand_in: &'static [u8] = include_bytes!("../fonts/Roboto-Variable.ttf");
        let system = SystemFonts {
            ui: Some(stand_in),
            mono: Some(stand_in),
        };
        let fonts = font_definitions_with(system, REGULAR);
        let proportional = &fonts.families[&FontFamily::Proportional];
        assert_eq!(
            &proportional[..2],
            [SF_PRO, ROBOTO],
            "SF Pro, then Roboto for missing glyphs"
        );
        assert_eq!(
            fonts.families[&FontFamily::Monospace]
                .first()
                .map(String::as_str),
            Some(MENLO)
        );
    }

    #[test]
    fn studio_falls_back_to_regular_roboto() {
        // Without the macOS system fonts (other platforms), Roboto leads.
        let fonts = font_definitions_with(SystemFonts::default(), UI_FONT_WEIGHT);
        assert_eq!(fonts.families[&FontFamily::Proportional][0], "Roboto");
    }

    #[test]
    fn the_whole_studio_uses_the_night_palette() {
        let ctx = Context::default();
        install_style(&ctx);
        let visuals = &ctx.style_of(egui::Theme::Dark).visuals;
        // Behind panels: the darkest surface, so panels read as cards on it.
        assert_eq!(visuals.panel_fill, NIGHT.shell);
        assert_eq!(visuals.window_fill, NIGHT.raised);
        assert_eq!(visuals.selection.bg_fill, NIGHT.accent_fill);
        assert_eq!(visuals.widgets.noninteractive.fg_stroke.color, NIGHT.text);
        // Soft surfaces: widgets are filled, never outlined.
        for widget in [
            &visuals.widgets.inactive,
            &visuals.widgets.hovered,
            &visuals.widgets.active,
        ] {
            assert_eq!(widget.bg_stroke, egui::Stroke::NONE);
        }
    }
}
