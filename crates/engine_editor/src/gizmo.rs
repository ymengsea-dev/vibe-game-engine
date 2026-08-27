//! Translate gizmo: three colored axis handles drawn at the selected
//! entity's position, draggable to move it along an axis.
//!
//! Screen-space math ([`project_to_screen`], [`pick_axis`],
//! [`drag_delta_along_axis`]) is pure and unit-tested; wiring it to a
//! live egui `Response` and a [`crate::Viewport`]'s camera lives in
//! [`crate::EditorShell::run_frame`] — the same pure-logic/UI split
//! every other panel in this crate uses.
//!
//! Only translation — rotate/scale handles are future work. Translation
//! is the most fundamentally useful of the three and the one every other
//! gizmo interaction builds on, and one draggable interaction at
//! production quality is a more honest scope for one iteration than
//! three sketched-in ones.
//!
//! Draws at the selected entity's raw `Transform` position, using
//! [`crate::Viewport`]'s own camera — the same position its placeholder
//! cube is drawn at (see [`crate::EditorState::entity_transforms`]), so
//! the gizmo sits exactly on the entity it's moving.

use glam::{Mat4, Vec2, Vec3};

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
}
