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

/// The disclosure chevron of the asset and Hierarchy trees: "›" when
/// closed, turning to "⌄" as `openness` goes from 0 to 1.
pub(crate) fn paint_chevron(
    painter: &egui::Painter,
    center: egui::Pos2,
    openness: f32,
    color: egui::Color32,
) {
    let angle = openness.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let points = [
        egui::vec2(-2.0, -4.0),
        egui::vec2(2.0, 0.0),
        egui::vec2(-2.0, 4.0),
    ]
    .map(|point| center + egui::vec2(point.x * cos - point.y * sin, point.x * sin + point.y * cos));
    painter.add(egui::Shape::line(
        points.to_vec(),
        egui::Stroke::new(1.5, color),
    ));
}

/// The icon of a drop-down (`egui::ComboBox::icon`): the same chevron as
/// the trees, pointing down, instead of egui's filled triangle.
pub(crate) fn combo_chevron(
    ui: &egui::Ui,
    rect: egui::Rect,
    _visuals: &egui::style::WidgetVisuals,
    _open: bool,
) {
    let color = if ui.is_enabled() {
        crate::theme::NIGHT.muted
    } else {
        crate::theme::NIGHT.subtle
    };
    paint_chevron(ui.painter(), rect.center(), 1.0, color);
}

/// A macOS-style checkbox: a soft rounded box that fills with the accent
/// colour and shows a white check when on. The whole row is clickable and
/// as tall as every other control.
pub(crate) fn checkbox(
    ui: &mut egui::Ui,
    palette: &Palette,
    on: &mut bool,
    label: &str,
) -> egui::Response {
    const BOX: f32 = 14.0;
    let height = ui.spacing().interact_size.y;
    let galley = (!label.is_empty()).then(|| {
        one_line(
            ui.painter(),
            label,
            12.0,
            palette.text,
            (ui.available_width() - BOX - 6.0).max(0.0),
        )
    });
    let width = BOX + galley.as_ref().map_or(0.0, |galley| 6.0 + galley.size().x);
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let box_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + BOX / 2.0, rect.center().y),
        egui::Vec2::splat(BOX),
    );
    let fill = match (*on, response.hovered() && enabled) {
        (true, _) => palette.accent,
        (false, true) => palette.selected.lerp_to_gamma(palette.muted, 0.25),
        (false, false) => palette.selected,
    };
    let painter = ui.painter();
    painter.rect_filled(
        box_rect,
        4.0,
        fill.gamma_multiply(if enabled { 1.0 } else { 0.5 }),
    );
    if *on {
        let c = box_rect.center();
        painter.add(egui::Shape::line(
            vec![
                c + egui::vec2(-3.5, 0.2),
                c + egui::vec2(-1.0, 2.7),
                c + egui::vec2(3.6, -2.6),
            ],
            egui::Stroke::new(1.7, palette.shell),
        ));
    }
    if let Some(galley) = galley {
        let pos = egui::pos2(
            box_rect.right() + 6.0,
            rect.center().y - galley.size().y / 2.0,
        );
        let color = if enabled {
            palette.text
        } else {
            palette.subtle
        };
        painter.galley(pos, galley, color);
    }
    response
}

/// A one-line text field as tall as every other control, filling the
/// width it is given.
pub(crate) fn text_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    let field = field(ui, text, 12.0, height)
        .hint_text(hint)
        .desired_width(ui.available_width());
    ui.add(field)
}

/// A one-line text edit in `size`-point text, `height` points tall. egui
/// sizes a text edit from its text and margins, so the margins make up the
/// height.
pub(crate) fn field<'t>(
    ui: &egui::Ui,
    text: &'t mut String,
    size: f32,
    height: f32,
) -> egui::TextEdit<'t> {
    let font = egui::FontId::proportional(size);
    let row = ui.ctx().fonts_mut(|fonts| fonts.row_height(&font));
    let vertical = ((height - row) / 2.0).round().clamp(0.0, 40.0) as i8;
    egui::TextEdit::singleline(text)
        .font(font)
        .margin(egui::Margin {
            left: 8,
            right: 8,
            top: vertical,
            bottom: vertical,
        })
        .vertical_align(egui::Align::Center)
}

/// A round "⋯" button for a section's less-used actions; show its menu
/// with `egui::Popup::menu(&response)`.
pub(crate) fn more_button(ui: &mut egui::Ui, palette: &Palette) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(22.0), egui::Sense::click());
    let painter = ui.painter();
    if response.hovered()
        || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response))
    {
        painter.rect_filled(rect, 11.0, palette.selected);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.muted
    };
    for dx in [-5.0, 0.0, 5.0] {
        painter.circle_filled(rect.center() + egui::vec2(dx, 0.0), 1.4, color);
    }
    response
}

/// A search field: a soft rounded field with a magnifier at the left,
/// `width` points wide and as tall as other controls.
pub(crate) fn search_field(
    ui: &mut egui::Ui,
    palette: &Palette,
    text: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    let mut edit = field(ui, text, 12.0, height)
        .hint_text(hint)
        .desired_width(width)
        .background_color(palette.hover);
    let margin = (height
        - ui.ctx()
            .fonts_mut(|fonts| fonts.row_height(&egui::FontId::proportional(12.0))))
        / 2.0;
    edit = edit.margin(egui::Margin {
        left: 26,
        right: 8,
        top: margin.round().clamp(0.0, 40.0) as i8,
        bottom: margin.round().clamp(0.0, 40.0) as i8,
    });
    let response = ui.add(edit);
    let center = egui::pos2(response.rect.left() + 13.0, response.rect.center().y - 0.5);
    let ink = egui::Stroke::new(1.4, palette.muted);
    ui.painter()
        .circle_stroke(center + egui::vec2(-1.0, -1.0), 4.0, ink);
    ui.painter().line_segment(
        [center + egui::vec2(2.0, 2.0), center + egui::vec2(5.0, 5.0)],
        ink,
    );
    response
}
