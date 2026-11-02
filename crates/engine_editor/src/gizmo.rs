//! The combined transform gizmo: move, rotate and scale from one set of
//! handles, so there is no mode to switch.
//!
//! | Handle | Drawn as | Drag to |
//! | --- | --- | --- |
//! | [`GizmoHandle::Move`] | An arrow per axis | Move along that axis |
//! | [`GizmoHandle::Rotate`] | A ring around the object per axis | Turn around that axis, like a dial |
//! | [`GizmoHandle::ScaleAxis`] | A small square on each arrow's shaft | Stretch the object along that axis |
//! | [`GizmoHandle::Scale`] | A square at the centre | Scale the object evenly |
//!
//! [`layout`] places every handle in screen space at a constant on-screen
//! size; the shell paints exactly that layout over the viewport image and
//! hit-tests it with [`pick`], so what you see is what you can grab. Rings
//! show only their front half (the half facing the camera) and only once the
//! pointer comes near ([`rings_revealed`]), so the gizmo reads as arrows with
//! scale knobs plus a centre square until you reach for a ring. The 2D view shows the X/Y
//! arrows and the Z ring only. All maths here is pure and
//! unit-tested; the shell supplies the camera matrix and the pointer.

use engine_utils::Transform;
use glam::{Mat4, Quat, Vec2, Vec3};

/// Smallest scale [`scale_uniform`] produces, so a drag cannot collapse or
/// flip the object.
pub const MIN_SCALE: f32 = 1.0e-3;

/// Scale change per pixel dragged on the centre square (right/up grows).
pub const SCALE_PER_PIXEL: f32 = 0.006;

/// On-screen length of every arrow, in pixels.
pub const HANDLE_LENGTH_PX: f32 = 90.0;

/// On-screen radius of every rotation ring, in pixels.
pub const RING_RADIUS_PX: f32 = 66.0;

/// Rings appear when the pointer comes within this distance of the
/// object's centre (or while one is dragged); otherwise only the arrows and
/// the centre square show, which keeps the gizmo uncluttered.
pub const RING_REVEAL_PX: f32 = 110.0;

/// Half the side of the centre scale square, in pixels.
pub const SCALE_HALF_SIZE_PX: f32 = 6.0;

/// How far along each arrow its scale knob sits, in pixels from the centre
/// (inside the rings, clear of the arrow head).
pub const SCALE_KNOB_PX: f32 = 50.0;

/// Half the side of an axis scale knob, in pixels.
pub const SCALE_KNOB_HALF_SIZE_PX: f32 = 4.5;

/// How close (in pixels) a pointer must be to a handle to pick it up.
pub const PICK_RADIUS_PX: f32 = 8.0;

/// Line segments per ring; enough to look round at the ring's size.
const RING_SEGMENTS: usize = 64;

/// A world axis.
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
    /// All three, in a fixed order.
    pub const ALL: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

    /// The unit direction this axis points in, world-space.
    pub fn direction(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
    }

    /// This axis's handle colour (RGBA, 0–1): the red/green/blue
    /// convention almost every 3D tool uses for X/Y/Z, in Tokyo Night's
    /// red, green and blue.
    pub fn color(self) -> [f32; 4] {
        match self {
            Axis::X => [0.97, 0.46, 0.56, 1.0],
            Axis::Y => [0.62, 0.81, 0.42, 1.0],
            Axis::Z => [0.48, 0.64, 0.97, 1.0],
        }
    }
}

/// One grabbable part of the gizmo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoHandle {
    /// The arrow along an axis.
    Move(Axis),
    /// The ring around an axis.
    Rotate(Axis),
    /// The knob on an arrow's shaft: scale along that axis.
    ScaleAxis(Axis),
    /// The centre square: uniform scale.
    Scale,
}

/// An arrow laid out on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenArrow {
    /// The axis this arrow moves along.
    pub axis: Axis,
    /// Screen position of the gizmo origin (pixels, viewport-local).
    pub start: Vec2,
    /// Screen position of the arrow tip.
    pub end: Vec2,
    /// World units moved per pixel dragged along the arrow.
    pub world_per_pixel: f32,
}

impl ScreenArrow {
    /// Screen position of this axis's scale knob, on the arrow's shaft.
    pub fn knob(&self) -> Vec2 {
        self.start + (self.end - self.start) * (SCALE_KNOB_PX / HANDLE_LENGTH_PX)
    }
}

/// A rotation ring laid out on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenRing {
    /// The axis this ring turns around.
    pub axis: Axis,
    /// The visible, camera-facing half of the ring as a polyline,
    /// viewport-local pixels (the whole ring when it faces the camera).
    pub points: Vec<Vec2>,
    /// `+1` if the axis points toward the camera (a counter-clockwise drag
    /// on screen is a positive turn), `-1` if it points away.
    pub facing: f32,
}

/// Every handle of the gizmo for one object, in screen space.
#[derive(Debug, Clone, PartialEq)]
pub struct GizmoLayout {
    /// Screen position of the object's origin.
    pub origin: Vec2,
    /// Move arrows.
    pub arrows: Vec<ScreenArrow>,
    /// Rotation rings.
    pub rings: Vec<ScreenRing>,
}

/// `transform` translated by `distance` world units along `axis`.
pub fn translate_transform(mut transform: Transform, axis: Axis, distance: f32) -> Transform {
    transform.translation += axis.direction() * distance;
    transform
}

/// `transform` rotated by `radians` about `axis` in world space
/// (pre-multiplied, so the axis is the world axis, not the object's own).
pub fn rotate_transform(mut transform: Transform, axis: Axis, radians: f32) -> Transform {
    transform.rotation =
        (Quat::from_axis_angle(axis.direction(), radians) * transform.rotation).normalize();
    transform
}

/// `transform` with its whole scale multiplied by `factor`, each axis kept
/// at least [`MIN_SCALE`].
pub fn scale_uniform(mut transform: Transform, factor: f32) -> Transform {
    if factor.is_finite() {
        transform.scale = (transform.scale * factor).max(Vec3::splat(MIN_SCALE));
    }
    transform
}

/// `transform` stretched by `factor` along the object's own axis that lines
/// up best with the world `axis` (exactly that axis for an unrotated object),
/// kept at least [`MIN_SCALE`]. Scale is stored per object axis, so this is
/// the closest a world-axis handle can get without shearing the object.
pub fn scale_along(mut transform: Transform, axis: Axis, factor: f32) -> Transform {
    if !factor.is_finite() {
        return transform;
    }
    let local = (transform.rotation.inverse() * axis.direction()).abs();
    let index = if local.x >= local.y && local.x >= local.z {
        0
    } else if local.y >= local.z {
        1
    } else {
        2
    };
    transform.scale[index] = (transform.scale[index] * factor).max(MIN_SCALE);
    transform
}

/// Projects `world` through `view_proj` into pixel coordinates within a
/// `screen_size`-sized viewport (origin top-left, `+Y` down — egui's
/// convention), or `None` if it's behind the camera.
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

/// The world-space direction from `point` toward the camera.
fn toward_camera(view_proj: Mat4, point: Vec3) -> Option<Vec3> {
    let inverse = view_proj.inverse();
    let clip = view_proj * point.extend(1.0);
    if clip.w <= 0.0 || !inverse.is_finite() {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    let near = inverse.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
    let direction = (near - point).normalize_or_zero();
    (direction != Vec3::ZERO).then_some(direction)
}

/// The handles shown in 3D and in 2D.
fn axes(two_d: bool) -> (&'static [Axis], &'static [Axis]) {
    if two_d {
        (&[Axis::X, Axis::Y], &[Axis::Z])
    } else {
        (&Axis::ALL, &Axis::ALL)
    }
}

/// Lays out the whole gizmo around `origin` at a constant on-screen size.
/// Returns `None` when the origin is behind the camera. An arrow pointing
/// almost straight at the camera has no usable screen direction and is left
/// out; so is a ring seen exactly edge-on.
pub fn layout(
    view_proj: Mat4,
    origin: Vec3,
    two_d: bool,
    screen_size: Vec2,
) -> Option<GizmoLayout> {
    let start = project_to_screen(view_proj, origin, screen_size)?;
    let (arrow_axes, ring_axes) = axes(two_d);
    let mut unit_pixels = 0.0f32;
    let arrows = arrow_axes
        .iter()
        .filter_map(|&axis| {
            // Probe one unit along the axis (closer if that is behind the
            // camera) to find its on-screen direction and scale.
            let (length, probe) = [1.0f32, 0.01].into_iter().find_map(|length| {
                project_to_screen(view_proj, origin + axis.direction() * length, screen_size)
                    .map(|probe| (length, probe))
            })?;
            let pixels = (probe - start).length();
            if !pixels.is_finite() || pixels < 1.0e-3 {
                return None;
            }
            unit_pixels = unit_pixels.max(pixels / length);
            Some(ScreenArrow {
                axis,
                start,
                end: start + (probe - start) / pixels * HANDLE_LENGTH_PX,
                world_per_pixel: length / pixels,
            })
        })
        .collect();
    let camera = toward_camera(view_proj, origin);
    // World radius that appears about RING_RADIUS_PX across on screen.
    let radius = if unit_pixels > 0.0 {
        RING_RADIUS_PX / unit_pixels
    } else {
        0.0
    };
    let rings = ring_axes
        .iter()
        .filter(|_| radius > 0.0)
        .filter_map(|&axis| ring(view_proj, origin, axis, radius, camera, screen_size))
        .collect();
    Some(GizmoLayout {
        origin: start,
        arrows,
        rings,
    })
}

fn ring(
    view_proj: Mat4,
    origin: Vec3,
    axis: Axis,
    radius: f32,
    camera: Option<Vec3>,
    screen_size: Vec2,
) -> Option<ScreenRing> {
    let normal = axis.direction();
    let (u, v) = normal.any_orthonormal_pair();
    // A ring seen edge-on collapses to a line and cannot be turned like a dial.
    let facing_amount = camera.map_or(1.0, |toward| normal.dot(toward));
    if facing_amount.abs() < 0.02 {
        return None;
    }
    // Sample from the point furthest from the camera, so the camera-facing
    // half is one continuous run in the middle. A ring that faces the camera
    // squarely (every offset sideways) keeps all of it.
    let toward = camera.unwrap_or(normal);
    let away_angle = toward.dot(v).atan2(toward.dot(u)) + std::f32::consts::PI;
    let face_on = facing_amount.abs() > 0.97;
    let points: Vec<Vec2> = (0..=RING_SEGMENTS)
        .filter_map(|step| {
            let angle = away_angle + step as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
            let offset = u * angle.cos() + v * angle.sin();
            let front = face_on || offset.dot(toward) >= -0.02;
            front
                .then(|| project_to_screen(view_proj, origin + offset * radius, screen_size))
                .flatten()
        })
        .collect();
    (points.len() >= 2).then_some(ScreenRing {
        axis,
        points,
        facing: facing_amount.signum(),
    })
}

/// Whether the rings should show: the pointer is near the gizmo's centre,
/// or a ring is being dragged.
pub fn rings_revealed(
    layout: &GizmoLayout,
    pointer: Option<Vec2>,
    dragging: Option<GizmoHandle>,
) -> bool {
    matches!(dragging, Some(GizmoHandle::Rotate(_)))
        || pointer.is_some_and(|pointer| pointer.distance(layout.origin) <= RING_REVEAL_PX)
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

/// The handle under `pointer`, if any. The centre square wins, then the
/// scale knobs, then the nearest arrow, then the nearest ring, so the small
/// handles stay grabbable where they overlap the larger ones. Rings count only when
/// `rings_shown` (see [`rings_revealed`]): a hidden ring cannot be grabbed.
pub fn pick(layout: &GizmoLayout, pointer: Vec2, rings_shown: bool) -> Option<GizmoHandle> {
    let offset = (pointer - layout.origin).abs();
    if offset.max_element() <= SCALE_HALF_SIZE_PX + 2.0 {
        return Some(GizmoHandle::Scale);
    }
    let knob_reach = SCALE_KNOB_HALF_SIZE_PX + 2.0;
    if let Some(arrow) = layout
        .arrows
        .iter()
        .find(|arrow| (pointer - arrow.knob()).abs().max_element() <= knob_reach)
    {
        return Some(GizmoHandle::ScaleAxis(arrow.axis));
    }
    let nearest = |candidates: Vec<(GizmoHandle, f32)>| {
        candidates
            .into_iter()
            .filter(|(_, distance)| *distance <= PICK_RADIUS_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(handle, _)| handle)
    };
    nearest(
        layout
            .arrows
            .iter()
            .map(|arrow| {
                (
                    GizmoHandle::Move(arrow.axis),
                    distance_to_segment(pointer, arrow.start, arrow.end),
                )
            })
            .collect(),
    )
    .or_else(|| {
        if !rings_shown {
            return None;
        }
        nearest(
            layout
                .rings
                .iter()
                .map(|ring| {
                    let distance = ring
                        .points
                        .windows(2)
                        .map(|pair| distance_to_segment(pointer, pair[0], pair[1]))
                        .fold(f32::INFINITY, f32::min);
                    (GizmoHandle::Rotate(ring.axis), distance)
                })
                .collect(),
        )
    })
}

/// Signed pixels `drag` moved along the arrow (positive toward its tip).
pub fn drag_along(arrow: &ScreenArrow, drag: Vec2) -> f32 {
    let direction = arrow.end - arrow.start;
    let length = direction.length();
    if length <= f32::EPSILON {
        return 0.0;
    }
    drag.dot(direction / length)
}

/// Radians to turn when the pointer moves from `previous` to `current`
/// around the screen-space `origin`, like turning a dial. `facing` is the
/// ring's [`ScreenRing::facing`], so the object turns the way the hand moves.
pub fn dial_angle(origin: Vec2, previous: Vec2, current: Vec2, facing: f32) -> f32 {
    // Flip y: screen space is y-down, angles are measured counter-clockwise.
    let before = Vec2::new(previous.x - origin.x, origin.y - previous.y);
    let after = Vec2::new(current.x - origin.x, origin.y - current.y);
    if before.length_squared() < 1.0 || after.length_squared() < 1.0 {
        return 0.0;
    }
    before.angle_to(after) * facing
}

/// The scale factor for a drag of `pixels` along an arrow: toward the tip
/// grows, toward the centre shrinks.
pub fn drag_axis_scale_factor(pixels: f32) -> f32 {
    (1.0 + pixels * SCALE_PER_PIXEL).max(0.01)
}

/// The scale factor for a drag on the centre square: right or up grows.
pub fn drag_scale_factor(drag: Vec2) -> f32 {
    (1.0 + (drag.x - drag.y) * SCALE_PER_PIXEL).max(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looking_at_origin() -> Mat4 {
        engine_renderer::Camera::new(Vec3::new(4.0, 3.0, 6.0), Vec3::ZERO, 1.0)
            .view_projection_matrix()
    }

    const SCREEN: Vec2 = Vec2::new(800.0, 800.0);

    #[test]
    fn project_to_screen_maps_ndc_corners_with_y_down() {
        assert_eq!(
            project_to_screen(Mat4::IDENTITY, Vec3::ZERO, Vec2::new(800.0, 600.0)),
            Some(Vec2::new(400.0, 300.0))
        );
        assert_eq!(
            project_to_screen(
                Mat4::IDENTITY,
                Vec3::new(1.0, 1.0, 0.0),
                Vec2::new(800.0, 600.0)
            ),
            Some(Vec2::new(800.0, 0.0))
        );
    }

    #[test]
    fn transforms_move_turn_and_scale_as_expected() {
        let moved = translate_transform(Transform::IDENTITY, Axis::Y, 3.0);
        assert_eq!(moved.translation, Vec3::new(0.0, 3.0, 0.0));
        let turned = rotate_transform(Transform::IDENTITY, Axis::Z, std::f32::consts::FRAC_PI_2);
        assert!((turned.rotation * Vec3::X - Vec3::Y).length() < 1e-5);
        let grown = scale_uniform(Transform::from_scale(Vec3::new(1.0, 2.0, 3.0)), 2.0);
        assert_eq!(grown.scale, Vec3::new(2.0, 4.0, 6.0));
        assert_eq!(
            scale_uniform(Transform::IDENTITY, -5.0).scale,
            Vec3::splat(MIN_SCALE)
        );
        assert_eq!(
            scale_uniform(Transform::IDENTITY, f32::NAN).scale,
            Vec3::ONE
        );
        let wide = scale_along(Transform::IDENTITY, Axis::X, 2.0);
        assert_eq!(wide.scale, Vec3::new(2.0, 1.0, 1.0));
        // Turned a quarter around Y, the object's own Z lies along world X.
        let turned = Transform {
            rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            ..Transform::IDENTITY
        };
        assert_eq!(
            scale_along(turned, Axis::X, 3.0).scale,
            Vec3::new(1.0, 1.0, 3.0)
        );
        assert_eq!(
            scale_along(Transform::IDENTITY, Axis::Y, -1.0).scale,
            Vec3::new(1.0, MIN_SCALE, 1.0)
        );
    }

    #[test]
    fn the_layout_has_arrows_rings_and_a_constant_screen_size() {
        let near = layout(looking_at_origin(), Vec3::ZERO, false, SCREEN).expect("in view");
        let far = layout(
            looking_at_origin(),
            Vec3::new(-20.0, 0.0, -30.0),
            false,
            SCREEN,
        )
        .expect("in view");
        assert_eq!(near.arrows.len(), 3);
        assert_eq!(near.rings.len(), 3);
        for arrow in near.arrows.iter().chain(&far.arrows) {
            assert!(((arrow.end - arrow.start).length() - HANDLE_LENGTH_PX).abs() < 1e-3);
        }
        let x = |l: &GizmoLayout| {
            l.arrows
                .iter()
                .find(|a| a.axis == Axis::X)
                .unwrap()
                .world_per_pixel
        };
        assert!(
            x(&far) > x(&near),
            "farther objects move further per pixel, tracking the pointer"
        );
        // Each ring stays roughly the same size on screen.
        for ring in &near.rings {
            let reach = ring
                .points
                .iter()
                .map(|point| point.distance(near.origin))
                .fold(0.0f32, f32::max);
            assert!(
                reach > RING_RADIUS_PX * 0.5 && reach < RING_RADIUS_PX * 3.0,
                "{reach}"
            );
        }
    }

    #[test]
    fn two_d_shows_x_and_y_arrows_and_only_the_z_ring() {
        let flat = engine_renderer::Camera::new_orthographic(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            1.0,
            10.0,
        )
        .view_projection_matrix();
        let layout = layout(flat, Vec3::ZERO, true, SCREEN).expect("in view");
        let arrows: Vec<_> = layout.arrows.iter().map(|arrow| arrow.axis).collect();
        let rings: Vec<_> = layout.rings.iter().map(|ring| ring.axis).collect();
        assert_eq!(arrows, vec![Axis::X, Axis::Y]);
        assert_eq!(rings, vec![Axis::Z]);
    }

    #[test]
    fn nothing_is_drawn_behind_the_camera_and_edge_on_rings_are_hidden() {
        assert!(
            layout(
                looking_at_origin(),
                Vec3::new(8.0, 6.0, 12.0),
                false,
                SCREEN
            )
            .is_none()
        );
        // Looking straight down -Z: the X and Y rings are edge-on lines.
        let front = engine_renderer::Camera::new(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 1.0)
            .view_projection_matrix();
        let layout = layout(front, Vec3::ZERO, false, SCREEN).expect("in view");
        let rings: Vec<_> = layout.rings.iter().map(|ring| ring.axis).collect();
        assert_eq!(rings, vec![Axis::Z]);
        assert_eq!(layout.rings[0].facing, 1.0, "+Z points at this camera");
    }

    #[test]
    fn rings_show_only_their_camera_facing_half() {
        let layout = layout(looking_at_origin(), Vec3::ZERO, false, SCREEN).expect("in view");
        for ring in &layout.rings {
            // Roughly half of the 65 samples survive for a ring seen at an angle.
            assert!(
                ring.points.len() < RING_SEGMENTS && ring.points.len() > RING_SEGMENTS / 3,
                "{:?}: {}",
                ring.axis,
                ring.points.len()
            );
        }
        let front = engine_renderer::Camera::new(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 1.0)
            .view_projection_matrix();
        let facing = layout_for(front);
        assert_eq!(
            facing.rings[0].points.len(),
            RING_SEGMENTS + 1,
            "a face-on ring is whole"
        );
    }

    fn layout_for(view_proj: Mat4) -> GizmoLayout {
        layout(view_proj, Vec3::ZERO, false, SCREEN).expect("in view")
    }

    #[test]
    fn rings_appear_only_near_the_gizmo_or_while_turning() {
        let layout = layout(looking_at_origin(), Vec3::ZERO, false, SCREEN).expect("in view");
        assert!(rings_revealed(
            &layout,
            Some(layout.origin + Vec2::new(60.0, 0.0)),
            None
        ));
        assert!(!rings_revealed(
            &layout,
            Some(layout.origin + Vec2::new(300.0, 0.0)),
            None
        ));
        assert!(!rings_revealed(&layout, None, None));
        assert!(rings_revealed(
            &layout,
            None,
            Some(GizmoHandle::Rotate(Axis::Y))
        ));
        assert!(!rings_revealed(
            &layout,
            None,
            Some(GizmoHandle::Move(Axis::Y))
        ));
    }

    #[test]
    fn picking_prefers_the_centre_then_knobs_then_arrows_then_rings() {
        let layout = layout(looking_at_origin(), Vec3::ZERO, false, SCREEN).expect("in view");
        assert_eq!(pick(&layout, layout.origin, true), Some(GizmoHandle::Scale));
        let x_arrow = layout.arrows.iter().find(|a| a.axis == Axis::X).unwrap();
        assert_eq!(
            pick(&layout, x_arrow.knob(), true),
            Some(GizmoHandle::ScaleAxis(Axis::X)),
            "the knob wins over the shaft it sits on"
        );
        assert!((x_arrow.knob().distance(x_arrow.start) - SCALE_KNOB_PX).abs() < 1e-3);
        let tip = x_arrow.start + (x_arrow.end - x_arrow.start) * 0.95;
        assert_eq!(pick(&layout, tip, true), Some(GizmoHandle::Move(Axis::X)));
        let ring = &layout.rings[0];
        let ring_point = ring.points[ring.points.len() / 2];
        assert!(matches!(
            pick(&layout, ring_point, true),
            Some(GizmoHandle::Rotate(_))
        ));
        assert_eq!(
            pick(&layout, layout.origin + Vec2::splat(300.0), true),
            None
        );
        assert!(
            !matches!(
                pick(&layout, ring_point, false),
                Some(GizmoHandle::Rotate(_))
            ),
            "a hidden ring cannot be grabbed"
        );
    }

    #[test]
    fn dragging_handles_maps_to_distance_angle_and_scale() {
        let arrow = ScreenArrow {
            axis: Axis::X,
            start: Vec2::ZERO,
            end: Vec2::new(90.0, 0.0),
            world_per_pixel: 0.1,
        };
        assert_eq!(drag_along(&arrow, Vec2::new(10.0, 5.0)), 10.0);
        assert_eq!(drag_along(&arrow, Vec2::new(-4.0, 0.0)), -4.0);

        // A quarter turn counter-clockwise on screen (y-down): right → up.
        let origin = Vec2::new(100.0, 100.0);
        let turn = dial_angle(origin, Vec2::new(150.0, 100.0), Vec2::new(100.0, 50.0), 1.0);
        assert!((turn - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "{turn}");
        let away = dial_angle(
            origin,
            Vec2::new(150.0, 100.0),
            Vec2::new(100.0, 50.0),
            -1.0,
        );
        assert!(
            (away + std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "axis facing away flips"
        );
        assert_eq!(
            dial_angle(origin, origin, Vec2::new(150.0, 100.0), 1.0),
            0.0
        );

        assert!(
            drag_scale_factor(Vec2::new(10.0, 0.0)) > 1.0,
            "drag right grows"
        );
        assert!(
            drag_scale_factor(Vec2::new(0.0, 10.0)) < 1.0,
            "drag down shrinks"
        );
        assert!(drag_scale_factor(Vec2::new(-1.0e6, 0.0)) > 0.0);
        assert!(drag_axis_scale_factor(10.0) > 1.0, "toward the tip grows");
        assert!(drag_axis_scale_factor(-10.0) < 1.0);
        assert!(drag_axis_scale_factor(-1.0e6) > 0.0);
    }
}
