//! Immediate-mode debug geometry: [`DebugDraw`] collects world-space
//! line segments for one frame and hands them to the renderer's
//! [`DebugLinePipeline`](crate::DebugLinePipeline) as a single vertex
//! list.
//!
//! Immediate mode, because that is what debug drawing is for: whatever
//! wants a line asks for one while it is looking at the data, and
//! nothing has to own or invalidate a retained buffer. The cost of that
//! is a [`DebugDraw::clear`] every frame, which is why the buffer keeps
//! its allocation across frames — a thousand colliders is a thousand
//! wire boxes redrawn sixty times a second, and reallocating that every
//! frame would be the overlay's whole cost.
//!
//! Everything here batches into **one** vertex list. The pipeline draws
//! it in a single call, whatever mix of boxes, spheres and frusta went
//! in, so the price of an overlay is the vertices it adds and not a draw
//! call per shape.

use glam::{Mat4, Vec3};

use crate::pipeline::DebugLineVertex;

/// How many segments a circle is drawn with. Twenty-four is round enough
/// at gizmo scale and keeps a sphere (three circles) at 144 vertices.
const CIRCLE_SEGMENTS: usize = 24;
const MAX_VERTICES: usize = 1_000_000;

/// The twelve edges of a box, as index pairs into its eight corners.
const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0),
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

/// A frame's worth of debug lines, in world space.
///
/// Fill it during the frame, hand [`DebugDraw::vertices`] to the
/// renderer, then [`DebugDraw::clear`] it.
#[derive(Debug, Default, Clone)]
pub struct DebugDraw {
    vertices: Vec<DebugLineVertex>,
}

impl DebugDraw {
    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops this frame's geometry, keeping the allocation for the next
    /// one.
    pub fn clear(&mut self) {
        self.vertices.clear();
    }

    /// The frame's vertices, as line-list pairs.
    pub fn vertices(&self) -> &[DebugLineVertex] {
        &self.vertices
    }

    /// Whether anything has been drawn this frame. An overlay set that
    /// is entirely switched off leaves this `true`, and the caller skips
    /// the draw call entirely.
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// How many vertices are queued (two per segment).
    pub fn len(&self) -> usize {
        self.vertices.len()
    }

    /// Appends one already-built vertex — for a caller that produces
    /// [`DebugLineVertex`]s of its own (the editor's gizmo handles) and
    /// wants them in the same batch as the overlays.
    pub fn push_vertex(&mut self, vertex: DebugLineVertex) {
        if self.vertices.len() < MAX_VERTICES {
            self.vertices.push(vertex);
        }
    }

    /// One segment from `start` to `end`.
    pub fn line(&mut self, start: Vec3, end: Vec3, color: [f32; 4]) {
        if self.vertices.len().saturating_add(2) > MAX_VERTICES {
            return;
        }
        self.vertices.push(DebugLineVertex {
            position: start.into(),
            color,
        });
        self.vertices.push(DebugLineVertex {
            position: end.into(),
            color,
        });
    }

    /// Draws connected line segments through the supplied points.
    pub fn polyline(&mut self, points: &[Vec3], color: [f32; 4]) {
        for pair in points.windows(2) {
            self.line(pair[0], pair[1], color);
        }
    }

    /// Draws the three coordinate axes from `origin`.
    pub fn axes(&mut self, origin: Vec3, length: f32) {
        self.line(
            origin,
            origin + Vec3::X * length.max(0.0),
            [1.0, 0.1, 0.1, 1.0],
        );
        self.line(
            origin,
            origin + Vec3::Y * length.max(0.0),
            [0.1, 1.0, 0.1, 1.0],
        );
        self.line(
            origin,
            origin + Vec3::Z * length.max(0.0),
            [0.1, 0.4, 1.0, 1.0],
        );
    }

    /// Draws a small cross marker at `point`.
    pub fn point(&mut self, point: Vec3, size: f32, color: [f32; 4]) {
        let size = size.max(0.0);
        self.line(point - Vec3::X * size, point + Vec3::X * size, color);
        self.line(point - Vec3::Y * size, point + Vec3::Y * size, color);
        self.line(point - Vec3::Z * size, point + Vec3::Z * size, color);
    }

    /// An axis-aligned wireframe box.
    pub fn wire_box(&mut self, centre: Vec3, half_extents: Vec3, color: [f32; 4]) {
        self.wire_box_transformed(Mat4::from_translation(centre), half_extents, color);
    }

    /// A wireframe box in the space of `transform` — for a collider that
    /// sits on a rotated entity.
    pub fn wire_box_transformed(&mut self, transform: Mat4, half_extents: Vec3, color: [f32; 4]) {
        let mut corners = [Vec3::ZERO; 8];
        for (index, corner) in corners.iter_mut().enumerate() {
            let sign = |bit: usize| if index & (1 << bit) == 0 { -1.0 } else { 1.0 };
            let local = Vec3::new(sign(0), sign(1), sign(2)) * half_extents;
            *corner = transform.transform_point3(local);
        }
        for &(a, b) in &BOX_EDGES {
            self.line(corners[a], corners[b], color);
        }
    }

    /// A wireframe sphere: three circles, one per axis plane.
    pub fn wire_sphere(&mut self, centre: Vec3, radius: f32, color: [f32; 4]) {
        self.circle(centre, radius, Vec3::X, Vec3::Y, color);
        self.circle(centre, radius, Vec3::Y, Vec3::Z, color);
        self.circle(centre, radius, Vec3::Z, Vec3::X, color);
    }

    /// A wireframe capsule along the Y axis: two hemisphere outlines and
    /// the sides joining them — a rough stand-in that reads correctly at
    /// a glance, which is all an overlay owes you.
    pub fn wire_capsule(&mut self, centre: Vec3, half_height: f32, radius: f32, color: [f32; 4]) {
        let top = centre + Vec3::Y * half_height;
        let bottom = centre - Vec3::Y * half_height;
        self.circle(top, radius, Vec3::X, Vec3::Z, color);
        self.circle(bottom, radius, Vec3::X, Vec3::Z, color);
        self.circle(top, radius, Vec3::X, Vec3::Y, color);
        self.circle(bottom, radius, Vec3::X, Vec3::Y, color);
        for axis in [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
            self.line(top + axis * radius, bottom + axis * radius, color);
        }
    }

    /// A wireframe cylinder along the Y axis.
    pub fn wire_cylinder(&mut self, centre: Vec3, half_height: f32, radius: f32, color: [f32; 4]) {
        let top = centre + Vec3::Y * half_height;
        let bottom = centre - Vec3::Y * half_height;
        self.circle(top, radius, Vec3::X, Vec3::Z, color);
        self.circle(bottom, radius, Vec3::X, Vec3::Z, color);
        for axis in [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z] {
            self.line(top + axis * radius, bottom + axis * radius, color);
        }
    }

    /// A camera's view volume, from its combined view-projection matrix.
    ///
    /// The eight clip-space corners are pushed back through the inverse,
    /// so this draws whatever the camera actually sees — including a
    /// skewed or orthographic projection — rather than a cone assumed
    /// from its parameters.
    pub fn frustum(&mut self, view_projection: Mat4, color: [f32; 4]) {
        let inverse = view_projection.inverse();
        let mut corners = [Vec3::ZERO; 8];
        for (index, corner) in corners.iter_mut().enumerate() {
            let sign = |bit: usize| if index & (1 << bit) == 0 { -1.0 } else { 1.0 };
            // wgpu clip space: x,y in [-1,1], z in [0,1].
            let z = if index & 0b100 == 0 { 0.0 } else { 1.0 };
            let clip = glam::Vec4::new(sign(0), sign(1), z, 1.0);
            let world = inverse * clip;
            *corner = if world.w.abs() > f32::EPSILON {
                world.truncate() / world.w
            } else {
                world.truncate()
            };
        }
        for &(a, b) in &BOX_EDGES {
            self.line(corners[a], corners[b], color);
        }
    }

    /// A flat grid of `cells` by `cells` squares of `cell_size`, centred
    /// on `centre` and lying in the XZ plane.
    pub fn grid(&mut self, centre: Vec3, cells: u32, cell_size: f32, color: [f32; 4]) {
        let cells = cells.clamp(1, 256);
        let half = cells as f32 * cell_size * 0.5;
        for step in 0..=cells {
            let offset = step as f32 * cell_size - half;
            self.line(
                centre + Vec3::new(offset, 0.0, -half),
                centre + Vec3::new(offset, 0.0, half),
                color,
            );
            self.line(
                centre + Vec3::new(-half, 0.0, offset),
                centre + Vec3::new(half, 0.0, offset),
                color,
            );
        }
    }

    /// A three-axis cross — for a point light, an emitter, anything whose
    /// position matters and whose shape does not.
    pub fn cross(&mut self, centre: Vec3, size: f32, color: [f32; 4]) {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            self.line(centre - axis * size, centre + axis * size, color);
        }
    }

    /// An arrow from `origin` along `direction` — a directional light's
    /// heading, say. The head is four short barbs, which is enough to
    /// read direction without a cone's vertex count.
    pub fn arrow(&mut self, origin: Vec3, direction: Vec3, length: f32, color: [f32; 4]) {
        let Some(dir) = direction.try_normalize() else {
            return;
        };
        let tip = origin + dir * length;
        self.line(origin, tip, color);
        // Any vector not parallel to `dir` gives a stable barb plane.
        let side = if dir.dot(Vec3::Y).abs() > 0.9 {
            Vec3::X
        } else {
            Vec3::Y
        };
        let right = dir.cross(side).normalize();
        let up = right.cross(dir);
        let barb = length * 0.15;
        for offset in [right, -right, up, -up] {
            self.line(tip, tip - dir * barb + offset * barb * 0.6, color);
        }
    }

    /// One circle in the plane spanned by `u` and `v`.
    fn circle(&mut self, centre: Vec3, radius: f32, u: Vec3, v: Vec3, color: [f32; 4]) {
        let mut previous = centre + u * radius;
        for step in 1..=CIRCLE_SEGMENTS {
            let angle = step as f32 / CIRCLE_SEGMENTS as f32 * std::f32::consts::TAU;
            let point = centre + (u * angle.cos() + v * angle.sin()) * radius;
            self.line(previous, point, color);
            previous = point;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

    #[test]
    fn debug_draw_clears_each_frame() {
        let mut draw = DebugDraw::new();
        assert!(draw.is_empty());

        draw.wire_box(Vec3::ZERO, Vec3::splat(1.0), WHITE);
        assert_eq!(draw.len(), 24, "twelve edges, two vertices each");

        draw.clear();
        assert!(draw.is_empty(), "a frame's geometry does not survive it");
        assert_eq!(draw.len(), 0);
    }

    #[test]
    fn shapes_batch_into_one_vertex_buffer() {
        let mut draw = DebugDraw::new();
        draw.wire_box(Vec3::ZERO, Vec3::ONE, WHITE);
        let after_box = draw.len();
        draw.wire_sphere(Vec3::X, 1.0, WHITE);
        let after_sphere = draw.len();
        draw.frustum(Mat4::IDENTITY, WHITE);
        let after_frustum = draw.len();

        // Every shape appends to the same list — the renderer sees one
        // buffer and issues one draw, whatever the mix.
        assert!(after_box < after_sphere && after_sphere < after_frustum);
        assert_eq!(draw.vertices().len(), after_frustum);
        assert_eq!(
            draw.vertices().len() % 2,
            0,
            "a line list is always vertex pairs"
        );
    }

    #[test]
    fn a_wire_box_sits_where_it_was_asked_to() {
        let mut draw = DebugDraw::new();
        draw.wire_box(Vec3::new(5.0, 0.0, 0.0), Vec3::splat(1.0), WHITE);
        for vertex in draw.vertices() {
            assert!((vertex.position[0] - 5.0).abs() <= 1.0 + f32::EPSILON);
        }
    }

    #[test]
    fn a_frustum_follows_its_projection() {
        // An identity view-projection makes clip space world space, so
        // the corners are the unit cube's — x,y in [-1,1], z in [0,1].
        let mut draw = DebugDraw::new();
        draw.frustum(Mat4::IDENTITY, WHITE);
        for vertex in draw.vertices() {
            assert!(vertex.position[0].abs() <= 1.0 + 1e-5);
            assert!(vertex.position[1].abs() <= 1.0 + 1e-5);
            assert!((0.0..=1.0).contains(&vertex.position[2]));
        }
    }

    #[test]
    fn an_arrow_with_no_direction_draws_nothing() {
        let mut draw = DebugDraw::new();
        draw.arrow(Vec3::ZERO, Vec3::ZERO, 1.0, WHITE);
        assert!(draw.is_empty(), "a zero direction has no arrow to draw");
    }

    #[test]
    fn general_shapes_batch_and_grid_divisions_are_bounded() {
        let mut draw = DebugDraw::new();
        draw.polyline(&[Vec3::ZERO, Vec3::X, Vec3::Y], WHITE);
        draw.axes(Vec3::ZERO, 1.0);
        draw.point(Vec3::ZERO, 0.5, WHITE);
        draw.grid(Vec3::ZERO, u32::MAX, 1.0, WHITE);
        assert!(draw.len() <= 2 * (2 * 257 + 2 + 3 + 3));
        assert_eq!(draw.len() % 2, 0);
    }
}
