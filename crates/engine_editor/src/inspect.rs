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

impl InspectField for f32 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        ui.add(egui::DragValue::new(self).speed(speed)).changed()
    }
}

impl InspectField for String {
    fn inspect_field(&mut self, ui: &mut egui::Ui, _speed: f32) -> bool {
        ui.text_edit_singleline(self).changed()
    }
}

impl InspectField for bool {
    fn inspect_field(&mut self, ui: &mut egui::Ui, _speed: f32) -> bool {
        ui.checkbox(self, "").changed()
    }
}

impl InspectField for Vec2 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        let mut changed = false;
        changed |= ui
            .add(egui::DragValue::new(&mut self.x).speed(speed))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut self.y).speed(speed))
            .changed();
        changed
    }
}

impl InspectField for Vec3 {
    fn inspect_field(&mut self, ui: &mut egui::Ui, speed: f32) -> bool {
        let mut changed = false;
        changed |= ui
            .add(egui::DragValue::new(&mut self.x).speed(speed))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut self.y).speed(speed))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut self.z).speed(speed))
            .changed();
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
        let mut degrees = Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees());
        let changed = degrees.inspect_field(ui, speed.max(1.0));
        if changed {
            *self = Quat::from_euler(
                EulerRot::XYZ,
                degrees.x.to_radians(),
                degrees.y.to_radians(),
                degrees.z.to_radians(),
            );
        }
        changed
    }
}

impl Inspectable for engine_utils::Transform {
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Translation");
            changed |= self.translation.inspect_field(ui, 0.05);
        });
        ui.horizontal(|ui| {
            ui.label("Rotation °");
            changed |= self.rotation.inspect_field(ui, 1.0);
        });
        ui.horizontal(|ui| {
            ui.label("Scale");
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
        ui.horizontal(|ui| {
            ui.label("Eye");
            changed |= self.eye.inspect_field(ui, 0.05);
        });
        ui.horizontal(|ui| {
            ui.label("Target");
            changed |= self.target.inspect_field(ui, 0.05);
        });
        ui.horizontal(|ui| {
            ui.label("Up");
            changed |= self.up.inspect_field(ui, 0.01);
        });
        match &mut self.projection {
            engine_renderer::Projection::Perspective { fov_y_radians } => {
                let mut degrees = fov_y_radians.to_degrees();
                ui.horizontal(|ui| {
                    ui.label("FOV °");
                    if degrees.inspect_field(ui, 0.5) {
                        *fov_y_radians = degrees.clamp(1.0, 179.0).to_radians();
                        changed = true;
                    }
                });
            }
            engine_renderer::Projection::Orthographic { height } => {
                ui.horizontal(|ui| {
                    ui.label("Ortho Height");
                    changed |= height.inspect_field(ui, 0.1);
                });
            }
        }
        ui.horizontal(|ui| {
            ui.label("Near");
            changed |= self.near.inspect_field(ui, 0.01);
        });
        ui.horizontal(|ui| {
            ui.label("Far");
            changed |= self.far.inspect_field(ui, 1.0);
        });
        changed
    }
}

/// Two `0.0..=1.0`-clamped drag values for a `[u, v]` atlas coordinate.
fn uv_pair(ui: &mut egui::Ui, uv: &mut [f32; 2]) -> bool {
    let mut changed = false;
    for component in uv.iter_mut() {
        if ui
            .add(egui::DragValue::new(component).speed(0.01).range(0.0..=1.0))
            .changed()
        {
            *component = component.clamp(0.0, 1.0);
            changed = true;
        }
    }
    changed
}

impl Inspectable for engine_ecs::components::Sprite {
    /// `size` in world units, `uv` as the atlas sub-rect (both corners,
    /// each `[u, v]` clamped to `0..=1`), and `color` as a linear RGBA
    /// tint picker.
    ///
    /// `Sprite` is the one renderer-adjacent component with a
    /// specialized editor for now: it is plain data. The mesh renderers
    /// (`MeshRenderer` / `SkinnedMeshRenderer` / …) hold live GPU
    /// handles and aren't editable as data until the asset-handle
    /// rework; `Collider` / `RigidBody` wrap opaque physics handles.
    fn inspect(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Size");
            changed |= self.size.inspect_field(ui, 0.05);
        });
        ui.horizontal(|ui| {
            ui.label("Atlas UV min");
            changed |= uv_pair(ui, &mut self.uv.min);
        });
        ui.horizontal(|ui| {
            ui.label("Atlas UV max");
            changed |= uv_pair(ui, &mut self.uv.max);
        });
        ui.horizontal(|ui| {
            ui.label("Tint");
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut self.color)
                .changed();
        });
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
