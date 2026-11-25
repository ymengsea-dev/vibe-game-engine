//! Solid shapes for building walkable areas: whether a shape fills the
//! space a character needs above a patch of floor, and the floors a
//! vertical line through a point lands on.
//!
//! Everything is in "grid space": `x` and `z` lie along the grid, `y` is
//! up. A caller whose world uses another plane (2D games walk on XY) maps
//! its shapes into this space first.

use glam::{Mat3, Mat4, Vec2, Vec3};

/// The space a walking character needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clearance {
    /// How far characters keep from solid things.
    pub radius: f32,
    /// The highest ledge characters step over without it blocking.
    pub step: f32,
    /// How tall characters are: anything lower overhead blocks.
    pub height: f32,
}

impl Clearance {
    /// The box over the cell `cell_min`–`cell_max` (grid `(x, z)`) with
    /// floor at `floor` that must stay empty for the cell to be walkable:
    /// grown by the radius, from the step height up to the character's
    /// height.
    pub fn space(&self, cell_min: Vec2, cell_max: Vec2, floor: f32) -> (Vec3, Vec3) {
        // Shrunk a hair so a shape that only touches the cell's edge
        // leaves it open.
        let grow = self.radius.max(0.0) - EDGE;
        // A shape whose top is the floor itself never blocks it, even with
        // no step height.
        let low = floor + self.step.max(FLOOR_GAP);
        let high = (floor + self.height).max(low);
        (
            Vec3::new(cell_min.x - grow, low, cell_min.y - grow),
            Vec3::new(cell_max.x + grow, high, cell_max.y + grow),
        )
    }
}

/// How far inside its edges a cell's space starts.
const EDGE: f32 = 1e-4;

/// The least gap kept between a floor and the space above it.
const FLOOR_GAP: f32 = 0.01;

/// A solid shape in grid space.
#[derive(Debug, Clone, PartialEq)]
pub enum Solid {
    /// A box: the cube from `-1` to `1` on each axis through `to_world`, so
    /// the matrix carries the half sizes. Any rotation or scale.
    Box {
        /// Unit cube to grid space.
        to_world: Mat4,
    },
    /// Every point within `radius` of the segment from `a` to `b`. A sphere
    /// has `a == b`.
    Capsule {
        /// One end of the core segment.
        a: Vec3,
        /// The other end.
        b: Vec3,
        /// Distance from the segment to the surface.
        radius: f32,
    },
    /// The cylinder of radius `1` round local Y, from `y = -1` to `1`,
    /// through `to_world`.
    Cylinder {
        /// Unit cylinder to grid space.
        to_world: Mat4,
    },
    /// Triangles, wound counter-clockwise seen from outside.
    Mesh(TriangleMesh),
}

impl Solid {
    /// The axis-aligned box from `min` to `max`.
    pub fn aabb(min: Vec3, max: Vec3) -> Self {
        let (lo, hi) = (min.min(max), min.max(max));
        Solid::Box {
            to_world: Mat4::from_translation((lo + hi) * 0.5)
                * Mat4::from_scale(((hi - lo) * 0.5).max(Vec3::splat(1e-6))),
        }
    }

    /// A ball of `radius` round `centre`.
    pub fn sphere(centre: Vec3, radius: f32) -> Self {
        Solid::Capsule {
            a: centre,
            b: centre,
            radius,
        }
    }

    /// The axis-aligned box around the shape.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        match self {
            Solid::Box { to_world } => {
                let centre = to_world.w_axis.truncate();
                let axes = Mat3::from_mat4(*to_world);
                let reach = axes.x_axis.abs() + axes.y_axis.abs() + axes.z_axis.abs();
                (centre - reach, centre + reach)
            }
            Solid::Capsule { a, b, radius } => {
                let r = Vec3::splat(radius.max(0.0));
                (a.min(*b) - r, a.max(*b) + r)
            }
            Solid::Cylinder { to_world } => {
                let centre = to_world.w_axis.truncate();
                let axes = Mat3::from_mat4(*to_world);
                let (x, z) = (axes.x_axis, axes.z_axis);
                let round = Vec3::new(x.x.hypot(z.x), x.y.hypot(z.y), x.z.hypot(z.z));
                let reach = round + axes.y_axis.abs();
                (centre - reach, centre + reach)
            }
            Solid::Mesh(mesh) => (mesh.min, mesh.max),
        }
    }

    /// Whether the shape touches the axis-aligned box from `min` to `max`.
    /// Cylinders count by the smaller of their box and their capsule, so a
    /// tilted cylinder blocks a little more than its shape.
    pub fn overlaps_box(&self, min: Vec3, max: Vec3) -> bool {
        let (lo, hi) = self.bounds();
        if lo.cmpgt(max).any() || hi.cmplt(min).any() {
            return false;
        }
        match self {
            Solid::Box { to_world } => box_overlaps_box(*to_world, min, max),
            Solid::Capsule { a, b, radius } => {
                segment_box_distance_squared(*a, *b, min, max) <= radius * radius
            }
            Solid::Cylinder { to_world } => {
                let axes = Mat3::from_mat4(*to_world);
                let centre = to_world.w_axis.truncate();
                let radius = axes.x_axis.length().max(axes.z_axis.length());
                box_overlaps_box(*to_world, min, max)
                    && segment_box_distance_squared(
                        centre - axes.y_axis,
                        centre + axes.y_axis,
                        min,
                        max,
                    ) <= radius * radius
            }
            Solid::Mesh(mesh) => mesh.overlaps_box(min, max),
        }
    }

    /// Whether the shape fills any of the space a character on `floor`
    /// needs over the cell `cell_min`–`cell_max`.
    pub fn blocks_floor(
        &self,
        cell_min: Vec2,
        cell_max: Vec2,
        floor: f32,
        clearance: &Clearance,
    ) -> bool {
        let (min, max) = clearance.space(cell_min, cell_max, floor);
        self.overlaps_box(min, max)
    }

    /// [`Solid::blocks_floor`] for a floor on this shape's own top. A box,
    /// ball or cylinder never blocks its own top, even where it rises
    /// within the character's radius (a ramp). A mesh still blocks with
    /// its walls and undersides, and with floors no steeper than
    /// `walkable_cos` (the cosine of the steepest walkable slope) left out.
    pub fn blocks_own_floor(
        &self,
        cell_min: Vec2,
        cell_max: Vec2,
        floor: f32,
        clearance: &Clearance,
        walkable_cos: f32,
    ) -> bool {
        let Solid::Mesh(mesh) = self else {
            return false;
        };
        let (min, max) = clearance.space(cell_min, cell_max, floor);
        mesh.near(min, max).any(|triangle| {
            let normal = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
            let walkable = normal.try_normalize().is_some_and(|n| n.y >= walkable_cos);
            !walkable && triangle_overlaps_box(triangle, min, max)
        }) || mesh.contains((min + max) * 0.5)
    }

    /// Where a vertical line through grid `(x, z)` lands on the shape from
    /// above: each surface's height and its upward normal. Only boxes,
    /// cylinders and meshes have floors; balls and capsules are props.
    pub fn tops(&self, at: Vec2) -> Vec<(f32, Vec3)> {
        match self {
            Solid::Box { to_world } => box_top(*to_world, at).into_iter().collect(),
            Solid::Cylinder { to_world } => cylinder_top(*to_world, at).into_iter().collect(),
            Solid::Capsule { .. } => Vec::new(),
            Solid::Mesh(mesh) => mesh
                .vertical_hits(at)
                .into_iter()
                .filter(|(_, normal)| normal.y > 0.0)
                .collect(),
        }
    }
}

/// A triangle list with a coarse grid over `(x, z)` so lookups only test
/// nearby triangles.
#[derive(Debug, Clone, PartialEq)]
pub struct TriangleMesh {
    triangles: Vec<[Vec3; 3]>,
    min: Vec3,
    max: Vec3,
    /// Bucket side, in grid units.
    bucket: f32,
    columns: usize,
    rows: usize,
    /// Row-major triangle indices per bucket.
    buckets: Vec<Vec<u32>>,
}

/// Most buckets one mesh is split into.
const MAX_BUCKETS: usize = 64 * 64;

impl TriangleMesh {
    /// The mesh from its triangles. Triangles with a non-finite corner
    /// are dropped.
    pub fn new(triangles: Vec<[Vec3; 3]>) -> Self {
        let triangles: Vec<[Vec3; 3]> = triangles
            .into_iter()
            .filter(|t| t.iter().all(|p| p.is_finite()))
            .collect();
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for point in triangles.iter().flatten() {
            min = min.min(*point);
            max = max.max(*point);
        }
        if triangles.is_empty() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        let size = (max - min).max(Vec3::splat(1e-3));
        let side = (triangles.len() as f32).sqrt().clamp(1.0, 64.0);
        let bucket = (size.x.max(size.z) / side).max(1e-3);
        let columns = ((size.x / bucket).ceil() as usize).clamp(1, 64);
        let rows = ((size.z / bucket).ceil() as usize).clamp(1, MAX_BUCKETS / columns);
        let mut mesh = Self {
            triangles,
            min,
            max,
            bucket,
            columns,
            rows,
            buckets: vec![Vec::new(); columns * rows],
        };
        for (index, triangle) in mesh.triangles.iter().enumerate() {
            let lo = triangle[0].min(triangle[1]).min(triangle[2]);
            let hi = triangle[0].max(triangle[1]).max(triangle[2]);
            let (c0, c1, r0, r1) = mesh.bucket_range(lo, hi);
            for row in r0..=r1 {
                for column in c0..=c1 {
                    mesh.buckets[row * mesh.columns + column].push(index as u32);
                }
            }
        }
        mesh
    }

    /// The triangles.
    pub fn triangles(&self) -> &[[Vec3; 3]] {
        &self.triangles
    }

    fn bucket_range(&self, lo: Vec3, hi: Vec3) -> (usize, usize, usize, usize) {
        let cell = |value: f32, origin: f32, count: usize| {
            (((value - origin) / self.bucket).floor().max(0.0) as usize).min(count - 1)
        };
        (
            cell(lo.x, self.min.x, self.columns),
            cell(hi.x, self.min.x, self.columns),
            cell(lo.z, self.min.z, self.rows),
            cell(hi.z, self.min.z, self.rows),
        )
    }

    /// The triangles that may touch the `(x, z)` range of `lo`–`hi`. A
    /// triangle can appear more than once.
    fn near(&self, lo: Vec3, hi: Vec3) -> impl Iterator<Item = &[Vec3; 3]> {
        let (c0, c1, r0, r1) = self.bucket_range(lo, hi);
        (r0..=r1).flat_map(move |row| {
            (c0..=c1).flat_map(move |column| {
                self.buckets[row * self.columns + column]
                    .iter()
                    .filter_map(|index| self.triangles.get(*index as usize))
            })
        })
    }

    /// Every place a vertical line through `(x, z)` crosses the mesh, with
    /// the crossed triangle's unit normal.
    pub fn vertical_hits(&self, at: Vec2) -> Vec<(f32, Vec3)> {
        let point = Vec3::new(at.x, 0.0, at.y);
        if at.x < self.min.x || at.x > self.max.x || at.y < self.min.z || at.y > self.max.z {
            return Vec::new();
        }
        let mut hits: Vec<(f32, Vec3)> = self
            .near(point, point)
            .filter_map(|triangle| vertical_hit(triangle, at))
            .collect();
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        hits.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-5 && a.1.dot(b.1) > 0.999);
        hits
    }

    /// Whether `point` is inside the mesh: the nearest crossing above it is
    /// a top and the nearest below it a bottom. An open surface, such as a
    /// single floor plane, has no inside.
    pub fn contains(&self, point: Vec3) -> bool {
        let hits = self.vertical_hits(Vec2::new(point.x, point.z));
        let above = hits.iter().find(|(y, _)| *y > point.y);
        let below = hits.iter().rev().find(|(y, _)| *y < point.y);
        matches!((above, below), (Some((_, up)), Some((_, down))) if up.y > 0.0 && down.y < 0.0)
    }

    fn overlaps_box(&self, min: Vec3, max: Vec3) -> bool {
        self.near(min, max)
            .any(|triangle| triangle_overlaps_box(triangle, min, max))
            || self.contains((min + max) * 0.5)
    }
}

/// Where the vertical line through `(x, z)` crosses `triangle`, with its
/// unit normal. Lines along an edge or a wall-on triangle miss.
fn vertical_hit(triangle: &[Vec3; 3], at: Vec2) -> Option<(f32, Vec3)> {
    let [a, b, c] = *triangle;
    let normal = (b - a).cross(c - a);
    if normal.y.abs() < 1e-9 {
        return None;
    }
    let flat = |p: Vec3| Vec2::new(p.x, p.z);
    let (pa, pb, pc) = (flat(a), flat(b), flat(c));
    let area = (pb - pa).perp_dot(pc - pa);
    let w0 = (pb - at).perp_dot(pc - at) / area;
    let w1 = (pc - at).perp_dot(pa - at) / area;
    let w2 = 1.0 - w0 - w1;
    let eps = -1e-6;
    if w0 < eps || w1 < eps || w2 < eps {
        return None;
    }
    Some((w0 * a.y + w1 * b.y + w2 * c.y, normal.normalize()))
}

/// The line `origin + t * direction` in the space of `to_world`'s unit
/// shape, with `t` the grid height.
fn local_line(to_world: Mat4, at: Vec2) -> Option<(Vec3, Vec3, Mat4)> {
    if to_world.determinant().abs() < 1e-12 {
        return None;
    }
    let inverse = to_world.inverse();
    let origin = inverse.transform_point3(Vec3::new(at.x, 0.0, at.y));
    let direction = inverse.transform_vector3(Vec3::Y);
    Some((origin, direction, inverse))
}

/// The upward normal in grid space of a local face normal.
fn world_normal(inverse: Mat4, local: Vec3) -> Vec3 {
    let normal = Mat3::from_mat4(inverse).transpose() * local;
    normal.try_normalize().unwrap_or(Vec3::Y)
}

/// The top of a box under a vertical line, if the line passes through it.
fn box_top(to_world: Mat4, at: Vec2) -> Option<(f32, Vec3)> {
    let (origin, direction, inverse) = local_line(to_world, at)?;
    let mut enter = f32::MIN;
    let mut exit = (f32::MAX, Vec3::Y);
    for axis in 0..3 {
        let (o, d) = (origin[axis], direction[axis]);
        if d.abs() < 1e-12 {
            if o.abs() > 1.0 {
                return None;
            }
            continue;
        }
        let (t0, t1) = ((-1.0 - o) / d, (1.0 - o) / d);
        enter = enter.max(t0.min(t1));
        if t0.max(t1) < exit.0 {
            let mut face = Vec3::ZERO;
            face[axis] = d.signum();
            exit = (t0.max(t1), face);
        }
    }
    (enter <= exit.0 && exit.0 < f32::MAX).then(|| (exit.0, world_normal(inverse, exit.1)))
}

/// The top of a cylinder under a vertical line, if the line passes through
/// it.
fn cylinder_top(to_world: Mat4, at: Vec2) -> Option<(f32, Vec3)> {
    let (origin, direction, inverse) = local_line(to_world, at)?;
    // Through the caps.
    let (mut enter, mut exit, mut exit_face) = (f32::MIN, f32::MAX, Vec3::Y);
    if direction.y.abs() < 1e-12 {
        if origin.y.abs() > 1.0 {
            return None;
        }
    } else {
        let (t0, t1) = (
            (-1.0 - origin.y) / direction.y,
            (1.0 - origin.y) / direction.y,
        );
        enter = t0.min(t1);
        exit = t0.max(t1);
        exit_face = Vec3::new(0.0, direction.y.signum(), 0.0);
    }
    // Through the round side.
    let (o, d) = (
        Vec2::new(origin.x, origin.z),
        Vec2::new(direction.x, direction.z),
    );
    let a = d.length_squared();
    if a < 1e-12 {
        if o.length_squared() > 1.0 {
            return None;
        }
    } else {
        let b = o.dot(d);
        let c = o.length_squared() - 1.0;
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        let root = disc.sqrt();
        let (t0, t1) = ((-b - root) / a, (-b + root) / a);
        enter = enter.max(t0);
        if t1 < exit {
            exit = t1;
            let side = o + d * t1;
            exit_face = Vec3::new(side.x, 0.0, side.y);
        }
    }
    (enter <= exit && exit < f32::MAX).then(|| (exit, world_normal(inverse, exit_face)))
}

/// Whether the parallelepiped `to_world` maps the unit cube onto touches
/// the axis-aligned box from `min` to `max` (separating axes).
fn box_overlaps_box(to_world: Mat4, min: Vec3, max: Vec3) -> bool {
    let centre = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let axes = Mat3::from_mat4(to_world);
    let columns = [axes.x_axis, axes.y_axis, axes.z_axis];
    let offset = to_world.w_axis.truncate() - centre;
    let separated = |axis: Vec3| {
        if axis.length_squared() < 1e-12 {
            return false;
        }
        let reach_a: f32 = columns.iter().map(|c| c.dot(axis).abs()).sum();
        let reach_b = half.dot(axis.abs());
        offset.dot(axis).abs() > reach_a + reach_b
    };
    let world = [Vec3::X, Vec3::Y, Vec3::Z];
    let faces = [
        columns[1].cross(columns[2]),
        columns[2].cross(columns[0]),
        columns[0].cross(columns[1]),
    ];
    if world
        .iter()
        .chain(faces.iter())
        .any(|axis| separated(*axis))
    {
        return false;
    }
    !world
        .iter()
        .any(|w| columns.iter().any(|c| separated(w.cross(*c))))
}

/// Whether a triangle touches the axis-aligned box from `min` to `max`
/// (separating axes: the box's sides, the triangle's plane, and the nine
/// edge crossings).
fn triangle_overlaps_box(triangle: &[Vec3; 3], min: Vec3, max: Vec3) -> bool {
    let centre = (min + max) * 0.5;
    let half = (max - min) * 0.5;
    let points = triangle.map(|p| p - centre);
    let separated = |axis: Vec3| {
        if axis.length_squared() < 1e-12 {
            return false;
        }
        let projected = points.map(|p| p.dot(axis));
        let low = projected[0].min(projected[1]).min(projected[2]);
        let high = projected[0].max(projected[1]).max(projected[2]);
        let reach = half.dot(axis.abs());
        low > reach || high < -reach
    };
    let edges = [
        points[1] - points[0],
        points[2] - points[1],
        points[0] - points[2],
    ];
    let world = [Vec3::X, Vec3::Y, Vec3::Z];
    if world.iter().any(|axis| separated(*axis)) || separated(edges[0].cross(edges[1])) {
        return false;
    }
    !world
        .iter()
        .any(|w| edges.iter().any(|edge| separated(w.cross(*edge))))
}

/// The squared distance from `point` to the box from `min` to `max`.
fn point_box_distance_squared(point: Vec3, min: Vec3, max: Vec3) -> f32 {
    point.distance_squared(point.clamp(min, max))
}

/// The squared distance from the segment `a`–`b` to the box from `min` to
/// `max`. The distance along the segment is convex, so a ternary search
/// finds its least value.
fn segment_box_distance_squared(a: Vec3, b: Vec3, min: Vec3, max: Vec3) -> f32 {
    let at = |t: f32| point_box_distance_squared(a.lerp(b, t), min, max);
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..48 {
        let m1 = low + (high - low) / 3.0;
        let m2 = high - (high - low) / 3.0;
        if at(m1) <= at(m2) {
            high = m2;
        } else {
            low = m1;
        }
    }
    at((low + high) * 0.5).min(at(0.0)).min(at(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(min: [f32; 3], max: [f32; 3]) -> (Vec3, Vec3) {
        (Vec3::from_array(min), Vec3::from_array(max))
    }

    /// A closed axis-aligned box as twelve outward-wound triangles.
    fn box_mesh(min: Vec3, max: Vec3) -> TriangleMesh {
        let corner = |i: usize| {
            Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            )
        };
        let quads = [
            [0, 4, 6, 2], // -X
            [1, 3, 7, 5], // +X
            [0, 1, 5, 4], // -Y
            [2, 6, 7, 3], // +Y
            [0, 2, 3, 1], // -Z
            [4, 5, 7, 6], // +Z
        ];
        let mut triangles = Vec::new();
        for [a, b, c, d] in quads {
            triangles.push([corner(a), corner(b), corner(c)]);
            triangles.push([corner(a), corner(c), corner(d)]);
        }
        TriangleMesh::new(triangles)
    }

    #[test]
    fn a_ball_blocks_by_its_round_shape_not_its_box() {
        let ball = Solid::sphere(Vec3::ZERO, 1.0);
        let (min, max) = cube([0.8, -0.1, 0.8], [0.9, 0.1, 0.9]);
        assert!(
            !ball.overlaps_box(min, max),
            "the corner of its box is empty"
        );
        let (min, max) = cube([0.6, -0.1, 0.6], [0.7, 0.1, 0.7]);
        assert!(ball.overlaps_box(min, max));
    }

    #[test]
    fn a_capsule_blocks_along_its_length() {
        let capsule = Solid::Capsule {
            a: Vec3::new(0.0, 0.0, -2.0),
            b: Vec3::new(0.0, 0.0, 2.0),
            radius: 0.5,
        };
        let (min, max) = cube([0.3, -0.1, 1.9], [0.4, 0.1, 2.0]);
        assert!(capsule.overlaps_box(min, max));
        let (min, max) = cube([0.45, -0.1, 2.4], [0.5, 0.1, 2.5]);
        assert!(!capsule.overlaps_box(min, max), "past the rounded end");
    }

    #[test]
    fn a_turned_box_blocks_by_its_turned_shape() {
        // A 2x2 box turned 45 degrees: a diamond reaching 1.41 along X.
        let turned = Solid::Box {
            to_world: Mat4::from_rotation_y(std::f32::consts::FRAC_PI_4),
        };
        let (min, max) = cube([1.2, -0.1, -0.05], [1.3, 0.1, 0.05]);
        assert!(turned.overlaps_box(min, max), "the diamond's point");
        let (min, max) = cube([1.0, -0.1, 0.9], [1.1, 0.1, 1.0]);
        assert!(!turned.overlaps_box(min, max), "the box's corner is empty");
    }

    #[test]
    fn a_cylinder_is_round() {
        let cylinder = Solid::Cylinder {
            to_world: Mat4::IDENTITY,
        };
        let (min, max) = cube([0.8, -0.1, 0.8], [0.9, 0.1, 0.9]);
        assert!(!cylinder.overlaps_box(min, max));
        let (min, max) = cube([0.9, 0.8, -0.05], [1.0, 0.9, 0.05]);
        assert!(cylinder.overlaps_box(min, max), "flat-ended, not rounded");
    }

    #[test]
    fn a_mesh_blocks_where_its_triangles_and_inside_are() {
        let mesh = Solid::Mesh(box_mesh(Vec3::ZERO, Vec3::splat(4.0)));
        let (min, max) = cube([1.0, 1.0, 1.0], [1.5, 1.5, 1.5]);
        assert!(mesh.overlaps_box(min, max), "deep inside");
        let (min, max) = cube([3.9, 1.0, 1.0], [4.1, 1.5, 1.5]);
        assert!(mesh.overlaps_box(min, max), "across a side");
        let (min, max) = cube([4.5, 1.0, 1.0], [5.0, 1.5, 1.5]);
        assert!(!mesh.overlaps_box(min, max));
    }

    #[test]
    fn an_open_floor_mesh_has_no_inside() {
        let floor = TriangleMesh::new(vec![
            [
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(0.0, 3.0, 4.0),
                Vec3::new(4.0, 3.0, 4.0),
            ],
            [
                Vec3::new(0.0, 3.0, 0.0),
                Vec3::new(4.0, 3.0, 4.0),
                Vec3::new(4.0, 3.0, 0.0),
            ],
        ]);
        assert!(!floor.contains(Vec3::new(2.0, 1.0, 2.0)), "under the deck");
        let tops = Solid::Mesh(floor).tops(Vec2::new(1.0, 2.0));
        assert_eq!(tops.len(), 1);
        assert!((tops[0].0 - 3.0).abs() < 1e-5);
        assert!(tops[0].1.y > 0.99);
    }

    #[test]
    fn tops_find_box_and_ramp_surfaces() {
        let crate_box = Solid::aabb(Vec3::new(0.0, 0.0, 0.0), Vec3::new(2.0, 1.0, 2.0));
        let tops = crate_box.tops(Vec2::new(1.0, 1.0));
        assert_eq!(tops.len(), 1);
        assert!((tops[0].0 - 1.0).abs() < 1e-5);
        assert!(crate_box.tops(Vec2::new(3.0, 1.0)).is_empty());

        // A ramp tilted 20 degrees round Z: its top rises along X.
        let ramp = Solid::Box {
            to_world: Mat4::from_rotation_z(20f32.to_radians())
                * Mat4::from_scale(Vec3::new(3.0, 0.1, 1.0)),
        };
        let low = ramp.tops(Vec2::new(-1.0, 0.0));
        let high = ramp.tops(Vec2::new(1.0, 0.0));
        assert!(high[0].0 > low[0].0);
        let slope = low[0].1.y.acos().to_degrees();
        assert!((slope - 20.0).abs() < 0.5, "{slope}");
    }

    #[test]
    fn a_cylinder_top_is_its_cap() {
        let pillar = Solid::Cylinder {
            to_world: Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)),
        };
        let tops = pillar.tops(Vec2::new(0.5, 0.0));
        assert!((tops[0].0 - 2.0).abs() < 1e-5);
        assert!(pillar.tops(Vec2::new(0.8, 0.8)).is_empty());
        assert!(Solid::sphere(Vec3::ZERO, 1.0).tops(Vec2::ZERO).is_empty());
    }

    #[test]
    fn a_ramp_does_not_block_its_own_top() {
        let clearance = Clearance {
            radius: 0.4,
            step: 0.2,
            height: 1.8,
        };
        // 35 degrees: across a grown cell the ramp rises past the step.
        let ramp = Solid::Box {
            to_world: Mat4::from_rotation_z(35f32.to_radians())
                * Mat4::from_scale(Vec3::new(4.0, 0.1, 2.0)),
        };
        let floor = ramp.tops(Vec2::new(0.25, 0.25))[0].0;
        let (lo, hi) = (Vec2::ZERO, Vec2::splat(0.5));
        assert!(ramp.blocks_floor(lo, hi, floor, &clearance));
        assert!(!ramp.blocks_own_floor(lo, hi, floor, &clearance, 0.7));

        // A closed mesh box: its top never blocks, its inside does.
        let mesh = Solid::Mesh(box_mesh(Vec3::ZERO, Vec3::splat(2.0)));
        assert!(!mesh.blocks_own_floor(lo, hi, 2.0, &clearance, 0.7));
        assert!(mesh.blocks_own_floor(lo, hi, 0.0, &clearance, 0.7));
    }

    #[test]
    fn space_over_a_floor_starts_above_it() {
        let clearance = Clearance {
            radius: 0.5,
            step: 0.0,
            height: 2.0,
        };
        let (min, max) = clearance.space(Vec2::ZERO, Vec2::ONE, 1.0);
        assert!(min.distance(Vec3::new(-0.5, 1.0 + FLOOR_GAP, -0.5)) < 1e-3);
        assert!(max.distance(Vec3::new(1.5, 3.0, 1.5)) < 1e-3);
        let slab = Solid::aabb(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0));
        assert!(
            !slab.blocks_floor(Vec2::ZERO, Vec2::ONE, 1.0, &clearance),
            "its own top never blocks"
        );
    }
}
