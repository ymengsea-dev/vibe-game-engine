//! Snapping for the move, turn and scale handles, and dropping objects
//! onto the ground.
//!
//! A gizmo drag keeps its running total since the press ([`GizmoDrag`]),
//! not just each frame's change: rounding per frame would lose every small
//! movement, so the total is snapped and applied to the transforms the
//! objects had when the drag began.

use engine_ecs::prelude::Entity;
use engine_utils::Transform;
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::gizmo::{Axis, GizmoHandle};

/// Smallest step the bottom bar accepts, so a zero step can't divide.
pub const MIN_STEP: f32 = 0.001;

/// The Scene View's snapping settings, saved with the editor session.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapSettings {
    /// Whether handles snap without holding ⌥ (⌥ flips it for one drag).
    pub enabled: bool,
    /// Grid size for moving, in metres.
    pub move_step: f32,
    /// Angle step for turning, in degrees.
    pub turn_step: f32,
    /// Step for scaling, as a fraction of the starting size.
    pub scale_step: f32,
    /// Whether models dropped from Project Assets rest their bottom on the
    /// surface instead of their centre.
    pub drop_on_surface: bool,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            move_step: 0.5,
            turn_step: 15.0,
            scale_step: 0.1,
            drop_on_surface: true,
        }
    }
}

impl SnapSettings {
    /// These settings with every step finite and at least [`MIN_STEP`].
    pub fn sanitized(self) -> Self {
        let step = |value: f32, default: f32| {
            if value.is_finite() {
                value.max(MIN_STEP)
            } else {
                default
            }
        };
        let defaults = Self::default();
        Self {
            move_step: step(self.move_step, defaults.move_step),
            turn_step: step(self.turn_step, defaults.turn_step),
            scale_step: step(self.scale_step, defaults.scale_step),
            ..self
        }
    }

    /// Whether a drag snaps: the toggle, flipped while ⌥ is held.
    pub fn active(&self, alt_held: bool) -> bool {
        self.enabled != alt_held
    }
}

/// The Snap menu's controls. Returns whether anything changed.
pub fn show_controls(ui: &mut egui::Ui, snap: &mut SnapSettings) -> bool {
    let before = *snap;
    ui.checkbox(&mut snap.enabled, "Snap handles to steps")
        .on_hover_text("Hold ⌥ while dragging a handle to flip this for one drag");
    ui.add(
        egui::DragValue::new(&mut snap.move_step)
            .prefix("Move ")
            .suffix(" m")
            .speed(0.05)
            .range(MIN_STEP..=1000.0),
    );
    ui.add(
        egui::DragValue::new(&mut snap.turn_step)
            .prefix("Turn ")
            .suffix("°")
            .speed(1.0)
            .range(MIN_STEP..=180.0),
    );
    ui.add(
        egui::DragValue::new(&mut snap.scale_step)
            .prefix("Scale ")
            .speed(0.01)
            .range(MIN_STEP..=10.0),
    );
    ui.checkbox(
        &mut snap.drop_on_surface,
        "Dropped models stand on the surface",
    )
    .on_hover_text(
        "A model dragged in from Project Assets rests its bottom, not its centre, on the spot",
    );
    *snap = snap.sanitized();
    *snap != before
}

/// `value` rounded to the nearest multiple of `step` (unchanged for a
/// step too small or not finite).
pub fn round_to(value: f32, step: f32) -> f32 {
    if !step.is_finite() || step < MIN_STEP || !value.is_finite() {
        return value;
    }
    (value / step).round() * step
}

/// How far to move along an axis so a point that started at `start` and
/// was dragged `raw` units lands on the grid.
pub fn snap_move(start: f32, raw: f32, step: f32) -> f32 {
    round_to(start + raw, step) - start
}

/// `raw_radians` rounded to whole `step_degrees`.
pub fn snap_turn(raw_radians: f32, step_degrees: f32) -> f32 {
    round_to(raw_radians.to_degrees(), step_degrees).to_radians()
}

/// A scale factor rounded so the result is a whole number of `step`s of
/// the starting size, never below one step.
pub fn snap_scale(raw_factor: f32, step: f32) -> f32 {
    round_to(raw_factor, step).max(step.max(MIN_STEP))
}

/// One handle drag in progress: the objects it moves, where they started,
/// and the total change since the press.
#[derive(Debug, Clone, PartialEq)]
pub struct GizmoDrag {
    /// The handle being dragged.
    pub handle: GizmoHandle,
    /// Each moved object with its world transform at the press.
    pub start: Vec<(Entity, Transform)>,
    /// Running total: world units for a move, radians for a turn, a
    /// multiplied factor for scaling.
    pub total: f32,
}

impl GizmoDrag {
    /// A drag of `handle` over `start` with nothing changed yet.
    pub fn new(handle: GizmoHandle, start: Vec<(Entity, Transform)>) -> Self {
        let total = match handle {
            GizmoHandle::ScaleAxis(_) | GizmoHandle::Scale => 1.0,
            GizmoHandle::Move(_) | GizmoHandle::Rotate(_) => 0.0,
        };
        Self {
            handle,
            start,
            total,
        }
    }

    /// Adds one frame's change: added for moves and turns, multiplied for
    /// scaling.
    pub fn accumulate(&mut self, change: f32) {
        if !change.is_finite() {
            return;
        }
        match self.handle {
            GizmoHandle::ScaleAxis(_) | GizmoHandle::Scale => self.total *= change,
            GizmoHandle::Move(_) | GizmoHandle::Rotate(_) => self.total += change,
        }
    }

    /// The change to apply from the start transforms: the total, snapped
    /// when `snap` is set. Moves snap the first object's position to the
    /// grid and carry the others along by the same distance.
    pub fn applied(&self, snap: Option<&SnapSettings>) -> f32 {
        let Some(snap) = snap else {
            return self.total;
        };
        match self.handle {
            GizmoHandle::Move(axis) => {
                let start = self
                    .start
                    .first()
                    .map(|(_, transform)| axis_component(transform.translation, axis))
                    .unwrap_or(0.0);
                snap_move(start, self.total, snap.move_step)
            }
            GizmoHandle::Rotate(_) => snap_turn(self.total, snap.turn_step),
            GizmoHandle::ScaleAxis(_) | GizmoHandle::Scale => {
                snap_scale(self.total, snap.scale_step)
            }
        }
    }
}

fn axis_component(value: Vec3, axis: Axis) -> f32 {
    value.dot(axis.direction())
}

/// How far above its origin an object must stand so the bottom of its
/// `local_min_y` (the lowest point of its mesh, in its own space) touches
/// the ground, for an object of `transform`'s rotation and scale.
pub fn ground_offset(transform: &Transform, local_min: Vec3, local_max: Vec3) -> f32 {
    let corners = [
        Vec3::new(local_min.x, local_min.y, local_min.z),
        Vec3::new(local_max.x, local_min.y, local_min.z),
        Vec3::new(local_min.x, local_max.y, local_min.z),
        Vec3::new(local_max.x, local_max.y, local_min.z),
        Vec3::new(local_min.x, local_min.y, local_max.z),
        Vec3::new(local_max.x, local_min.y, local_max.z),
        Vec3::new(local_min.x, local_max.y, local_max.z),
        Vec3::new(local_max.x, local_max.y, local_max.z),
    ];
    let lowest = corners
        .iter()
        .map(|corner| (transform.rotation * (*corner * transform.scale)).y)
        .fold(f32::INFINITY, f32::min);
    if lowest.is_finite() { -lowest } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    #[test]
    fn rounds_to_steps() {
        assert_eq!(round_to(1.26, 0.5), 1.5);
        assert_eq!(round_to(-0.74, 0.5), -0.5);
        assert_eq!(round_to(3.0, 0.0), 3.0);
    }

    #[test]
    fn move_lands_on_the_grid_from_any_start() {
        // Started at 0.3, dragged 1.0 → 1.3 → nearest grid point 1.5.
        let moved = snap_move(0.3, 1.0, 0.5);
        assert!((0.3 + moved - 1.5).abs() < 1e-5);
    }

    #[test]
    fn turn_snaps_to_whole_degrees() {
        let snapped = snap_turn(20f32.to_radians(), 15.0);
        assert!((snapped.to_degrees() - 15.0).abs() < 1e-3);
    }

    #[test]
    fn scale_never_snaps_to_zero() {
        assert!((snap_scale(0.02, 0.1) - 0.1).abs() < 1e-6);
        assert!((snap_scale(1.27, 0.1) - 1.3).abs() < 1e-5);
    }

    #[test]
    fn alt_flips_the_toggle() {
        let snap = SnapSettings::default();
        assert!(!snap.active(false));
        assert!(snap.active(true));
        let on = SnapSettings {
            enabled: true,
            ..snap
        };
        assert!(on.active(false));
        assert!(!on.active(true));
    }

    #[test]
    fn drag_keeps_small_moves_until_they_reach_a_step() {
        let start = Transform::default();
        let mut drag = GizmoDrag::new(
            GizmoHandle::Move(Axis::X),
            vec![(Entity::PLACEHOLDER, start)],
        );
        let snap = SnapSettings::default();
        for _ in 0..10 {
            drag.accumulate(0.05);
        }
        // 10 × 0.05 = 0.5: one whole step, although each frame alone
        // would round to nothing.
        assert!((drag.applied(Some(&snap)) - 0.5).abs() < 1e-5);
        assert!((drag.applied(None) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn scale_drag_multiplies() {
        let mut drag = GizmoDrag::new(GizmoHandle::Scale, Vec::new());
        drag.accumulate(2.0);
        drag.accumulate(1.5);
        assert!((drag.total - 3.0).abs() < 1e-6);
    }

    #[test]
    fn ground_offset_lifts_a_centred_cube_by_half_its_height() {
        let transform = Transform::default();
        let offset = ground_offset(&transform, Vec3::splat(-1.0), Vec3::splat(1.0));
        assert!((offset - 1.0).abs() < 1e-6);
        let scaled = Transform {
            scale: Vec3::splat(2.0),
            ..transform
        };
        assert!((ground_offset(&scaled, Vec3::splat(-1.0), Vec3::splat(1.0)) - 2.0).abs() < 1e-6);
        let upside_down = Transform {
            rotation: Quat::from_rotation_x(std::f32::consts::PI),
            ..transform
        };
        let model = ground_offset(
            &upside_down,
            Vec3::new(-1.0, 0.0, -1.0),
            Vec3::new(1.0, 3.0, 1.0),
        );
        assert!((model - 3.0).abs() < 1e-4);
    }

    #[test]
    fn sanitize_fixes_bad_steps() {
        let bad = SnapSettings {
            move_step: 0.0,
            turn_step: f32::NAN,
            ..SnapSettings::default()
        }
        .sanitized();
        assert_eq!(bad.move_step, MIN_STEP);
        assert_eq!(bad.turn_step, 15.0);
    }
}
