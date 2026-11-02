//! The `Inspectable` / `InspectField` traits behind the Inspector panel.
//!
//! [`Inspectable`] is "this value has an egui property editor"; usually
//! `#[derive(Inspectable)]`d (see [`macro@Inspectable`]), which emits one
//! labelled row per field, each drawn through that field type's
//! [`InspectField`] impl. This crate provides [`InspectField`] for the
//! primitives an entity's editable components are made of (`f32`,
//! `String`, `bool`, [`glam::Vec3`], [`glam::Quat`]), plus hand-written
//! [`Inspectable`] impls for the foreign component types the derive can't
//! be attached to ([`engine_utils::Transform`]).

use glam::{EulerRot, Quat, Vec2, Vec3};

pub use engine_editor_derive::Inspectable;

/// A value with an egui property editor. Derive it for a named-field
/// struct, or implement it by hand for a type the derive can't reach.
pub trait Inspectable {
    /// Draws editors for this value's fields and returns whether the user
    /// changed anything this frame.
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool;
}

/// How a single field type renders as one inspector widget. The
/// [`Inspectable`] derive calls this per field.
pub trait InspectField {
    /// Draws this field, with `speed` as the drag sensitivity for numeric
    /// widgets (ignored by non-numeric ones). Returns whether it changed.
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool;
}

/// The widest the label column of a [`property_row`] gets.
pub const LABEL_WIDTH: f32 = 84.0;

/// Below this width a [`property_row`] puts its label above its controls
/// instead of beside them, so narrow panels stack rather than squeeze.
pub const STACK_BELOW: f32 = 250.0;

/// One labelled Inspector row: `label` in a fixed column on the left (so
/// every row's controls line up), `add_contents` laid out left to right in
/// the rest. Controls that don't fit wrap onto another line instead of
/// widening the panel; in a narrow panel the label goes above them.
pub fn property_row<R>(
    ui: &mut egui::Ui,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let palette = crate::theme::NIGHT;
    let height = ui.spacing().interact_size.y;
    let width = ui.available_width();
    let stacked = width < STACK_BELOW;
    let label_width = if stacked {
        width
    } else {
        (width * 0.3).clamp(56.0, LABEL_WIDTH)
    };
    let size = if stacked { 11.5 } else { 12.0 };
    let galley =
        crate::widgets::one_line(ui.painter(), label, size, palette.muted, label_width - 8.0);
    let origin = ui.cursor().min;
    let label_height = if stacked { galley.size().y + 2.0 } else { 0.0 };
    let at = if stacked {
        origin
    } else {
        egui::pos2(origin.x, origin.y + (height - galley.size().y) / 2.0)
    };
    ui.painter().galley(at, galley, palette.muted);
    let left = if stacked { 0.0 } else { label_width };
    let values = egui::Rect::from_min_max(
        egui::pos2(origin.x + left, origin.y + label_height),
        egui::pos2(origin.x + width, origin.y + label_height + height),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(values)
            .layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true)),
        add_contents,
    )
    .inner
}

/// The width of one of three equal cells in a [`property_row`], so single
/// numbers line up with the X / Y / Z columns of the rows around them.
pub fn cell_width(ui: &egui::Ui) -> f32 {
    let spacing = ui.spacing().item_spacing.x;
    ((ui.available_width() - 2.0 * spacing) / 3.0)
        .floor()
        .max(36.0)
}

/// A number field one [`cell_width`] wide.
pub fn number(ui: &mut egui::Ui, value: egui::DragValue<'_>) -> egui::Response {
    let size = egui::vec2(cell_width(ui), ui.spacing().interact_size.y);
    let response = narrow_padding(ui, |ui| ui.add_sized(size, value));
    scrub_cursor(ui, &response);
    response
}

/// Shows that a number field changes when dragged sideways: while the
/// pointer is over it or dragging it, the system cursor is replaced by a
/// left-right arrow drawn above everything. Not while the field is being
/// typed in, where the text cursor belongs.
pub fn scrub_cursor(ui: &egui::Ui, response: &egui::Response) {
    if !(response.hovered() || response.dragged()) || response.has_focus() {
        return;
    }
    let Some(pointer) = ui.ctx().pointer_latest_pos() else {
        return;
    };
    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("inspector_scrub_cursor"),
    ));
    paint_scrub_cursor(&painter, pointer);
}

/// A left-right arrow with a bar through its middle, white on a dark
/// outline so it reads on any fill.
fn paint_scrub_cursor(painter: &egui::Painter, at: egui::Pos2) {
    let (left, right) = (at + egui::vec2(-9.0, 0.0), at + egui::vec2(9.0, 0.0));
    let strokes = [
        egui::Stroke::new(3.4, egui::Color32::from_black_alpha(170)),
        egui::Stroke::new(1.6, egui::Color32::WHITE),
    ];
    for stroke in strokes {
        painter.line_segment([left, right], stroke);
        painter.line_segment(
            [at + egui::vec2(0.0, -6.0), at + egui::vec2(0.0, 6.0)],
            stroke,
        );
        for (tip, direction) in [(left, 1.0), (right, -1.0)] {
            painter.line_segment([tip, tip + egui::vec2(4.0 * direction, -4.0)], stroke);
            painter.line_segment([tip, tip + egui::vec2(4.0 * direction, 4.0)], stroke);
        }
    }
}

/// Number fields that fill the row in equal cells, each marked X, Y or Z.
fn axis_fields(ui: &mut egui::Ui, values: &mut [f32], speed: f32, suffix: &str) -> bool {
    let count = values.len() as f32;
    let spacing = ui.spacing().item_spacing.x;
    let width = ((ui.available_width() - spacing * (count - 1.0)) / count)
        .floor()
        .max(36.0);
    let size = egui::vec2(width, ui.spacing().interact_size.y);
    let mut changed = false;
    for (value, axis) in values.iter_mut().zip(["X", "Y", "Z"]) {
        let response = narrow_padding(ui, |ui| {
            ui.add_sized(
                size,
                egui::DragValue::new(value).speed(speed).suffix(suffix),
            )
        });
        changed |= response.changed();
        scrub_cursor(ui, &response);
        if !response.has_focus() {
            let color = crate::theme::NIGHT.subtle;
            let galley = crate::widgets::one_line(ui.painter(), axis, 11.0, color, 12.0);
            let at = egui::pos2(
                response.rect.left() + 7.0,
                response.rect.center().y - galley.size().y / 2.0,
            );
            ui.painter().galley(at, galley, color);
        }
    }
    changed
}

/// Less side padding, so a short number keeps its cell's width instead of
/// growing it.
fn narrow_padding<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let padding = ui.spacing().button_padding;
    ui.spacing_mut().button_padding.x = 4.0;
    let inner = add(ui);
    ui.spacing_mut().button_padding = padding;
    inner
}

impl InspectField for f32 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        number(ui, egui::DragValue::new(self).speed(speed)).changed()
    }
}

impl InspectField for String {
    fn inspect_field(&mut self, ui: &mut egui::Ui, _speed: f32) -> bool {
        crate::widgets::text_field(ui, self, "").changed()
    }
}

impl InspectField for bool {
    fn inspect_field(&mut self, ui: &mut egui::Ui, _speed: f32) -> bool {
        crate::widgets::checkbox(ui, &crate::theme::NIGHT, self, "").changed()
    }
}

impl InspectField for Vec2 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        let mut values = self.to_array();
        let changed = axis_fields(ui, &mut values, speed, "");
        *self = Vec2::from_array(values);
        changed
    }
}

impl InspectField for Vec3 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        let mut values = self.to_array();
        let changed = axis_fields(ui, &mut values, speed, "");
        *self = Vec3::from_array(values);
        changed
    }
}

impl InspectField for Quat {
    /// Edited as XYZ Euler angles in degrees — far easier to reason about
    /// by hand than raw quaternion components. Round-tripping through
    /// Euler is lossy for extreme orientations, the standard trade-off
    /// every engine's transform inspector makes.
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        let (x, y, z) = self.to_euler(EulerRot::XYZ);
        // `+ 0.0` turns a -0 into 0, so an unrotated object doesn't show "-0°".
        let mut degrees = [x, y, z].map(|angle| angle.to_degrees() + 0.0);
        let changed = axis_fields(ui, &mut degrees, speed.max(1.0), "°");
        if changed {
            *self = Quat::from_euler(
                EulerRot::XYZ,
                degrees[0].to_radians(),
                degrees[1].to_radians(),
                degrees[2].to_radians(),
            );
        }
        changed
    }
}

impl Inspectable for engine_utils::Transform {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        property_row(ui, "Position", |ui| {
            changed |= self.translation.inspect_field(ui, 0.05);
        });
        property_row(ui, "Rotation", |ui| {
            changed |= self.rotation.inspect_field(ui, 1.0);
        });
        property_row(ui, "Scale", |ui| {
            changed |= self.scale.inspect_field(ui, 0.05);
        });
        changed
    }
}

impl Inspectable for engine_renderer::Camera {
    /// Edits eye / target / up, near / far, and — depending on the
    /// projection mode — either the vertical FOV (degrees) or the
    /// orthographic height. `aspect_ratio` is left out: the viewport
    /// owns it.
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        property_row(ui, "Eye", |ui| {
            changed |= self.eye.inspect_field(ui, 0.05);
        });
        property_row(ui, "Target", |ui| {
            changed |= self.target.inspect_field(ui, 0.05);
        });
        property_row(ui, "Up", |ui| {
            changed |= self.up.inspect_field(ui, 0.01);
        });
        match &mut self.projection {
            engine_renderer::Projection::Perspective { fov_y_radians } => {
                let mut degrees = fov_y_radians.to_degrees();
                property_row(ui, "Field of view", |ui| {
                    if degrees.inspect_field(ui, 0.5) {
                        *fov_y_radians = degrees.clamp(1.0, 179.0).to_radians();
                        changed = true;
                    }
                });
            }
            engine_renderer::Projection::Orthographic { height } => {
                property_row(ui, "Height", |ui| {
                    changed |= height.inspect_field(ui, 0.1);
                });
            }
        }
        property_row(ui, "Near", |ui| {
            changed |= self.near.inspect_field(ui, 0.01);
        });
        property_row(ui, "Far", |ui| {
            changed |= self.far.inspect_field(ui, 1.0);
        });
        changed
    }
}

impl Inspectable for engine_ecs::components::Sprite {
    /// `size` in world units, `color` as a linear RGBA tint picker,
    /// and an explicit draw layer. Atlas UVs are managed by the image packer.
    ///
    /// `Sprite` is the one renderer-adjacent component with a
    /// specialized editor for now: it is plain data. The mesh renderers
    /// (`MeshRenderer` / `SkinnedMeshRenderer` / …) hold live GPU
    /// handles and aren't editable as data until the asset-handle
    /// rework; `Collider` / `RigidBody` wrap opaque physics handles.
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        property_row(ui, "Size", |ui| {
            changed |= self.size.inspect_field(ui, 0.05);
        });
        property_row(ui, "Tint", |ui| {
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut self.color)
                .changed();
        });
        property_row(ui, "Layer", |ui| {
            changed |= number(ui, egui::DragValue::new(&mut self.z_order).speed(0.1)).changed();
        });
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `scrub-cursor.png`: a position row with the drag cursor over
    /// its Y field, for a visual check. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_scrub_cursor() {
        let path = crate::ui_snapshot::snapshot_dir().join("scrub-cursor.png");
        let mut position = Vec3::new(0.0, 0.681, -2.5);
        crate::ui_snapshot::render_png(&path, egui::vec2(320.0, 70.0), 3.0, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, crate::theme::NIGHT.raised);
            ui.add_space(20.0);
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut field = egui::Rect::NOTHING;
            property_row(ui, "Position", |ui| {
                position.inspect_field(ui, 0.05);
                field = ui.min_rect();
            });
            let cell = (field.width() - 12.0) / 3.0;
            let at = egui::pos2(field.left() + cell * 1.5 + 14.0, field.center().y);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("snapshot_cursor"),
            ));
            paint_scrub_cursor(&painter, at);
        })
        .expect("snapshot");
    }

    #[derive(Inspectable)]
    struct DemoLight {
        intensity: f32,
        #[inspect(label = "Casts Shadows")]
        casts_shadows: bool,
        #[inspect(skip)]
        _cached_lumens: f32,
    }

    #[test]
    fn derived_inspect_runs_and_reports_no_change_without_input() {
        let mut light = DemoLight {
            intensity: 1.0,
            casts_shadows: true,
            _cached_lumens: 0.0,
        };
        let mut changed = true;
        egui::__run_test_ui(|ui| {
            changed = light.inspect(ui);
        });
        assert!(!changed, "no user input this frame");
    }

    #[test]
    fn hand_impl_for_transform_runs() {
        let mut transform = engine_utils::Transform::IDENTITY;
        let mut changed = true;
        egui::__run_test_ui(|ui| {
            changed = transform.inspect(ui);
        });
        assert!(!changed);
    }

    #[test]
    fn hand_impl_for_camera_runs_without_change() {
        let mut camera =
            engine_renderer::Camera::new(Vec3::new(0.0, 2.0, 6.0), Vec3::ZERO, 16.0 / 9.0);
        let before = camera;
        let mut changed = true;
        egui::__run_test_ui(|ui| {
            changed = camera.inspect(ui);
        });
        assert!(!changed);
        assert_eq!(
            camera, before,
            "an untouched pass must not drift the camera"
        );
    }

    #[test]
    fn vec2_field_round_trips_an_unchanged_value() {
        let mut value = Vec2::new(3.0, -1.0);
        let before = value;
        egui::__run_test_ui(|ui| {
            value.inspect_field(ui, 0.1);
        });
        assert_eq!(value, before);
    }

    #[test]
    fn sprite_inspectable_runs_and_reports_no_change() {
        let mut sprite = engine_ecs::components::Sprite::new(
            Vec2::splat(2.0),
            engine_renderer::UvRect {
                min: [0.1, 0.1],
                max: [0.9, 0.9],
            },
        );
        let before = sprite;
        let mut changed = true;
        egui::__run_test_ui(|ui| {
            changed = sprite.inspect(ui);
        });
        assert!(!changed);
        assert_eq!(
            sprite, before,
            "an untouched pass must not drift the sprite"
        );
    }

    #[test]
    fn quat_field_round_trips_an_unchanged_rotation_without_drift() {
        // `inspect_field` only rewrites `self` when the user changes a
        // value; an untouched pass must leave the quaternion exactly as
        // it was.
        let mut rotation = Quat::from_rotation_y(0.7);
        let before = rotation;
        egui::__run_test_ui(|ui| {
            rotation.inspect_field(ui, 1.0);
        });
        assert_eq!(rotation, before);
    }
}
