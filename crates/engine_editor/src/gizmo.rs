//! Transform gizmo: three colored axis handles drawn at the selected
//! entity's position, draggable to translate, rotate, or scale it along
//! an axis depending on the active [`GizmoMode`].
//!
//! Screen-space math ([`project_to_screen`], [`pick_axis`],
//! [`drag_delta_along_axis`]) and the per-mode transform edits
//! ([`translate_transform`], [`rotate_transform`], [`scale_transform`])
//! are pure and unit-tested; wiring them to a live egui `Response` and a
//! [`crate::Viewport`]'s camera lives in [`crate::EditorShell::run_frame`]
//! — the same pure-logic/UI split every other panel in this crate uses.
//!
//! All three modes share the same straight axis-line handles (drawn by
//! [`crate::Viewport`]); the mode only changes what a drag along a handle
//! *does*. Distinct rotate-ring and scale-box handle shapes are future
//! polish.
//!
//! Draws at the selected entity's raw `Transform` position, using
//! [`crate::Viewport`]'s own camera — the same position its placeholder
//! cube is drawn at (see [`crate::EditorState::entity_transforms`]), so
//! the gizmo sits exactly on the entity it's editing.

use engine_utils::Transform;
use glam::{Mat4, Quat, Vec2, Vec3};

/// Which kind of edit a drag along a gizmo handle performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GizmoMode {
    /// Drag moves the entity along the axis.
    #[default]
    Translate,
    /// Drag spins the entity about the axis.
    Rotate,
    /// Drag grows/shrinks the entity's scale on the axis.
    Scale,
}

impl GizmoMode {
    /// All three, in toolbar order.
    pub const ALL: [GizmoMode; 3] = [GizmoMode::Translate, GizmoMode::Rotate, GizmoMode::Scale];

    /// Short label for a toolbar button.
    pub fn label(self) -> &'static str {
        match self {
            GizmoMode::Translate => "Move",
            GizmoMode::Rotate => "Rotate",
            GizmoMode::Scale => "Scale",
        }
    }
}

/// Radians of rotation per pixel of drag along a handle, in
/// [`GizmoMode::Rotate`].
pub const ROTATE_RADIANS_PER_PIXEL: f32 = 0.01;

/// Scale-factor change per pixel of drag along a handle, in
/// [`GizmoMode::Scale`] (a drag of `+100 px` multiplies that axis's scale
/// by `1 + 100 * this`).
pub const SCALE_PER_PIXEL: f32 = 0.01;

/// Smallest per-axis scale [`scale_transform`] will produce — keeps a
/// drag from collapsing an axis to zero or flipping it negative.
pub const MIN_SCALE: f32 = 1.0e-3;

/// Which axis a gizmo handle, or a drag along one, refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Red.
    X,
    /// Green.
    Y,
    /// Blue.
    Z,
}

impl Axis {
    /// All three, in a fixed order — for iterating "every handle".
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    /// The unit direction this axis points in, world-space.
    pub fn direction(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }

    /// This axis's handle color — linear RGBA, matching the red/green/
    /// blue convention almost every 3D tool uses for X/Y/Z.
    pub fn color(self) -> [f32; 4] {
        match self {
            Axis::X => [0.9, 0.2, 0.2, 1.0],
            Axis::Y => [0.2, 0.85, 0.2, 1.0],
            Axis::Z => [0.25, 0.45, 0.95, 1.0],
        }
    }
}

/// Which gizmo handles to show/hit-test for `mode`, honouring 2D
/// editing conventions when `two_d` is set:
/// - 3D → all three axes;
/// - 2D translate / scale → X and Y (the screen plane);
/// - 2D rotate → Z only (rotation about the view axis).
pub fn axes_for(mode: GizmoMode, two_d: bool) -> &'static [Axis] {
    if !two_d {
        return &Axis::ALL;
    }
    match mode {
        GizmoMode::Rotate => &[Axis::Z],
        GizmoMode::Translate | GizmoMode::Scale => &[Axis::X, Axis::Y],
    }
}

/// How far each handle extends from the gizmo's origin, in world units.
pub const HANDLE_LENGTH: f32 = 1.0;

/// How close (in pixels) a pointer must be to a handle's screen-space
/// line to pick it up — a generous target, since a 1-pixel-wide line is
/// otherwise very hard to click precisely.
pub const PICK_RADIUS_PX: f32 = 8.0;

/// `axis`'s handle as a world-space line segment, from `origin`.
pub fn axis_endpoints(origin: Vec3, axis: Axis) -> (Vec3, Vec3) {
    (origin, origin + axis.direction() * HANDLE_LENGTH)
}

/// `transform` translated by `distance` world units along `axis`.
pub fn translate_transform(mut transform: Transform, axis: Axis, distance: f32) -> Transform {
    transform.translation += axis.direction() * distance;
    transform
}

/// `transform` rotated by `radians` about `axis` in world space
/// (pre-multiplied, so the axis is the world axis, not the entity's local
/// one).
pub fn rotate_transform(mut transform: Transform, axis: Axis, radians: f32) -> Transform {
    transform.rotation =
        (Quat::from_axis_angle(axis.direction(), radians) * transform.rotation).normalize();
    transform
}

/// `transform` with its scale on `axis` multiplied by `factor`, clamped
/// to at least [`MIN_SCALE`] so an axis can't collapse or invert.
pub fn scale_transform(mut transform: Transform, axis: Axis, factor: f32) -> Transform {
    let scaled = match axis {
        Axis::X => &mut transform.scale.x,
        Axis::Y => &mut transform.scale.y,
        Axis::Z => &mut transform.scale.z,
    };
    *scaled = (*scaled * factor).max(MIN_SCALE);
    transform
}

/// Projects `world` through `view_proj` into pixel coordinates within a
/// `screen_size`-sized viewport (origin top-left, `+Y` down — egui's
/// convention), or `None` if it's behind the camera (`w <= 0`, where the
/// perspective divide below would be meaningless).
pub fn project_to_screen(view_proj: Mat4, world: Vec3, screen_size: Vec2) -> Option<Vec2> {
    let clip = view_proj * world.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(Vec2::new(
        (ndc.x * 0.5 + 0.5) * screen_size.x,
        (1.0 - (ndc.y * 0.5 + 0.5)) * screen_size.y,
    ))
}

/// The shortest distance from `point` to the segment `a`-`b`.
fn distance_to_segment(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    let t = if len_sq > f32::EPSILON {
        ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.distance(a + ab * t)
}

/// Which of `handles` (each an axis with its projected screen-space
/// start/end) `pointer` is within [`PICK_RADIUS_PX`] pixels of — the
/// closest one, if more than one qualifies. `None` if none do.
pub fn pick_axis(handles: &[(Axis, Vec2, Vec2)], pointer: Vec2) -> Option<Axis> {
    handles
        .iter()
        .map(|&(axis, start, end)| (axis, distance_to_segment(pointer, start, end)))
        .filter(|&(_, distance)| distance <= PICK_RADIUS_PX)
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(axis, _)| axis)
}

/// Projects `drag_delta_px` (a screen-space mouse movement, in pixels)
/// onto the 2D screen-space direction from `screen_start` to
/// `screen_end`, returning a signed scalar: how far along that direction
/// the drag moved, in pixels (positive towards `screen_end`).
///
/// `0.0` if the axis projects to (nearly) a single point on screen —
/// looking straight down it, where no drag direction is meaningful.
pub fn drag_delta_along_axis(screen_start: Vec2, screen_end: Vec2, drag_delta_px: Vec2) -> f32 {
    let direction = screen_end - screen_start;
    let length = direction.length();
    if length <= f32::EPSILON {
        return 0.0;
    }
    drag_delta_px.dot(direction / length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    #[test]
    fn axis_endpoints_starts_at_origin_and_extends_one_unit() {
        let origin = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(
            axis_endpoints(origin, Axis::X),
            (origin, origin + Vec3::new(1.0, 0.0, 0.0))
        );
        assert_eq!(
            axis_endpoints(origin, Axis::Y),
            (origin, origin + Vec3::new(0.0, 1.0, 0.0))
        );
        assert_eq!(
            axis_endpoints(origin, Axis::Z),
            (origin, origin + Vec3::new(0.0, 0.0, 1.0))
        );
    }

    #[test]
    fn project_to_screen_maps_ndc_origin_to_screen_center() {
        let screen = Vec2::new(800.0, 600.0);
        let pixel = project_to_screen(Mat4::IDENTITY, Vec3::ZERO, screen).unwrap();
        assert_eq!(pixel, Vec2::new(400.0, 300.0));
    }

    #[test]
    fn project_to_screen_flips_y_for_top_left_pixel_origin() {
        // NDC (1, 1) is the top-right in OpenGL/wgpu's convention
        // (+Y up), which must land at pixel (width, 0) — the top-right
        // in screen space (+Y down).
        let screen = Vec2::new(800.0, 600.0);
        let pixel = project_to_screen(Mat4::IDENTITY, Vec3::new(1.0, 1.0, 0.0), screen).unwrap();
        assert_eq!(pixel, Vec2::new(800.0, 0.0));
    }

    #[test]
    fn project_to_screen_returns_none_behind_the_camera() {
        // A synthetic matrix (not a real camera projection) whose only
        // purpose is producing `w = z`, so a negative-`z` input yields
        // `w <= 0` — exactly the "behind the camera" case this function
        // must reject.
        let w_equals_z = Mat4::from_cols(
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 1.0),
            Vec4::new(0.0, 0.0, 0.0, 0.0),
        );
        let screen = Vec2::new(800.0, 600.0);

        assert_eq!(
            project_to_screen(w_equals_z, Vec3::new(0.0, 0.0, -1.0), screen),
            None
        );
        assert!(project_to_screen(w_equals_z, Vec3::new(0.0, 0.0, 1.0), screen).is_some());
    }

    #[test]
    fn pick_axis_finds_the_handle_within_range() {
        let handles = [
            (Axis::X, Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0)),
            (Axis::Y, Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0)),
        ];
        assert_eq!(pick_axis(&handles, Vec2::new(50.0, 2.0)), Some(Axis::X));
        assert_eq!(pick_axis(&handles, Vec2::new(2.0, 50.0)), Some(Axis::Y));
    }

    #[test]
    fn pick_axis_returns_none_when_nothing_is_close_enough() {
        let handles = [(Axis::X, Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0))];
        assert_eq!(pick_axis(&handles, Vec2::new(50.0, 500.0)), None);
    }

    #[test]
    fn pick_axis_prefers_the_closer_handle_when_several_are_in_range() {
        let handles = [
            (Axis::X, Vec2::new(0.0, 1.0), Vec2::new(100.0, 1.0)),
            (Axis::Y, Vec2::new(0.0, 4.0), Vec2::new(100.0, 4.0)),
        ];
        assert_eq!(pick_axis(&handles, Vec2::new(50.0, 0.0)), Some(Axis::X));
    }

    #[test]
    fn drag_delta_along_axis_projects_onto_a_horizontal_axis() {
        let start = Vec2::new(0.0, 0.0);
        let end = Vec2::new(100.0, 0.0);
        assert_eq!(
            drag_delta_along_axis(start, end, Vec2::new(5.0, 100.0)),
            5.0
        );
    }

    #[test]
    fn drag_delta_along_axis_is_negative_moving_away_from_the_end() {
        let start = Vec2::new(0.0, 0.0);
        let end = Vec2::new(100.0, 0.0);
        assert_eq!(
            drag_delta_along_axis(start, end, Vec2::new(-5.0, 0.0)),
            -5.0
        );
    }

    #[test]
    fn drag_delta_along_axis_is_zero_for_a_degenerate_axis() {
        let point = Vec2::new(10.0, 10.0);
        assert_eq!(
            drag_delta_along_axis(point, point, Vec2::new(5.0, 5.0)),
            0.0
        );
    }

    // --- per-mode transform edits ---

    #[test]
    fn translate_transform_moves_along_the_axis_only() {
        let out = translate_transform(Transform::IDENTITY, Axis::Y, 3.0);
        assert_eq!(out.translation, Vec3::new(0.0, 3.0, 0.0));
        assert_eq!(out.rotation, Quat::IDENTITY);
        assert_eq!(out.scale, Vec3::ONE);
    }

    #[test]
    fn rotate_transform_spins_about_the_world_axis() {
        let out = rotate_transform(Transform::IDENTITY, Axis::Z, std::f32::consts::FRAC_PI_2);
        // +90° about Z turns +X into +Y.
        let turned = out.rotation * Vec3::X;
        assert!((turned - Vec3::Y).length() < 1e-5);
        assert!(
            (out.rotation.length() - 1.0).abs() < 1e-6,
            "stays normalized"
        );
    }

    #[test]
    fn rotate_transform_composes_on_top_of_existing_rotation() {
        let start = rotate_transform(Transform::IDENTITY, Axis::Y, 0.5);
        let more = rotate_transform(start, Axis::Y, 0.5);
        let expected = Quat::from_axis_angle(Vec3::Y, 1.0);
        assert!(more.rotation.dot(expected).abs() > 1.0 - 1e-5);
    }

    #[test]
    fn scale_transform_multiplies_one_axis() {
        let out = scale_transform(Transform::from_scale(Vec3::splat(2.0)), Axis::X, 1.5);
        assert_eq!(out.scale, Vec3::new(3.0, 2.0, 2.0));
    }

    #[test]
    fn axes_for_follows_2d_conventions() {
        for mode in GizmoMode::ALL {
            assert_eq!(axes_for(mode, false), &Axis::ALL, "3D shows every axis");
        }
        assert_eq!(axes_for(GizmoMode::Translate, true), &[Axis::X, Axis::Y]);
        assert_eq!(axes_for(GizmoMode::Scale, true), &[Axis::X, Axis::Y]);
        assert_eq!(axes_for(GizmoMode::Rotate, true), &[Axis::Z]);
    }

    #[test]
    fn scale_transform_clamps_to_a_positive_minimum() {
        let out = scale_transform(Transform::IDENTITY, Axis::Z, -5.0);
        assert_eq!(out.scale.z, MIN_SCALE);
        assert_eq!(out.scale.x, 1.0);
    }

    #[test]
    fn gizmo_mode_default_is_translate() {
        assert_eq!(GizmoMode::default(), GizmoMode::Translate);
        assert_eq!(GizmoMode::ALL.len(), 3);
    }
}
