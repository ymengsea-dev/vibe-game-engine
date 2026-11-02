//! Studio's shared controls, in the style described in
//! `docs/dev/04_UI_STYLE.md`: soft filled shapes, no outlines, one palette
//! ([`crate::theme::NIGHT`]). Use these instead of styling a new control
//! by hand, so every panel stays consistent.

use crate::theme::Palette;

/// `text` on one line at most `max_width` wide, ending in "…" if cut.
pub(crate) fn one_line(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    painter.layout_job(job)
}

/// A soft pill button, macOS-style: filled, no outline.
pub(crate) fn pill_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    text: &str,
    color: egui::Color32,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(text).size(12.5).color(color))
            .fill(palette.hover)
            .stroke(egui::Stroke::NONE)
            .corner_radius(6)
            .min_size(egui::vec2(0.0, 24.0)),
    )
}

/// A macOS-style on/off switch.
pub(crate) fn toggle(ui: &mut egui::Ui, palette: &Palette, on: &mut bool) -> egui::Response {
    let size = egui::vec2(34.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let mix = |from: u8, to: u8| egui::lerp(f32::from(from)..=f32::from(to), how_on) as u8;
    let track = egui::Color32::from_rgb(
        mix(palette.selected.r(), palette.accent.r()),
        mix(palette.selected.g(), palette.accent.g()),
        mix(palette.selected.b(), palette.accent.b()),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, rect.height() / 2.0, track);
    let radius = rect.height() / 2.0 - 2.0;
    let x = egui::lerp(
        (rect.left() + radius + 2.0)..=(rect.right() - radius - 2.0),
        how_on,
    );
    painter.circle_filled(egui::pos2(x, rect.center().y), radius, egui::Color32::WHITE);
    response
}

/// A macOS pop-up button ("30 days ⌄"): shows the current choice and opens
/// a short menu of `choices`. Returns the newly picked one.
pub(crate) fn popup_choice(
    ui: &mut egui::Ui,
    palette: &Palette,
    current: u32,
    choices: &[u32],
    unit: &str,
) -> Option<u32> {
    let mut picked = None;
    let label = format!("{current} {unit}");
    let ink_color = if ui.is_enabled() {
        palette.text
    } else {
        palette.subtle
    };
    let (button, _) = egui::containers::menu::MenuButton::new(
        egui::RichText::new(format!("{label}     "))
            .size(12.5)
            .color(ink_color),
    )
    .ui(ui, |ui| {
        for &choice in choices {
            let text = format!("{choice} {unit}");
            if ui.selectable_label(choice == current, text).clicked() {
                picked = Some(choice);
                ui.close();
            }
        }
    });
    // The chevron, drawn so it does not depend on a font having the glyph.
    let tip = button.rect.right_center() - egui::vec2(12.0, -1.5);
    let ink = egui::Stroke::new(
        1.3,
        if ui.is_enabled() {
            palette.muted
        } else {
            palette.subtle
        },
    );
    ui.painter()
        .line_segment([tip + egui::vec2(-3.5, -3.5), tip], ink);
    ui.painter()
        .line_segment([tip, tip + egui::vec2(3.5, -3.5)], ink);
    picked
}

/// The track of an Apple-style segmented control: a soft rounded inset
/// with no outline; the chosen segment is a lighter pill inside it.
pub(crate) fn segmented(palette: &Palette) -> egui::Frame {
    egui::Frame::NONE
        .fill(palette.shell)
        .corner_radius(9)
        .inner_margin(egui::Margin::same(2))
}

/// One text segment of a [`segmented`] control.
pub(crate) fn segment_label(
    ui: &mut egui::Ui,
    palette: &Palette,
    selected: bool,
    label: &str,
) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).size(12.5).color(if selected {
            palette.text
        } else {
            palette.muted
        }))
        .fill(if selected {
            palette.selected
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(egui::Stroke::NONE)
        .corner_radius(7)
        .min_size(egui::vec2(38.0, 22.0)),
    )
}
