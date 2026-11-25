//! Roads and paths: a smooth line through points an object carries, drawn as
//! a flat strip and pressed into the terrain below it.
//!
//! [`RoadData`] is the saved format. The line is a centripetal Catmull-Rom
//! spline through the points ([`RoadData::samples`]); [`RoadData::ribbon`]
//! turns it into the strip mesh games draw, and [`RoadShape`] answers, for
//! any ground point, how high the road is there and how strongly the ground
//! should follow it. Studio flattens and paints terrain with that; games
//! only draw the strip (the ground was flattened in Studio).

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::format::AssetRef;

/// Most points one road may have.
pub const MAX_ROAD_POINTS: usize = 256;
/// Widest a road (or its soft edge) may be, in world units.
pub const MAX_ROAD_WIDTH: f32 = 1_000.0;
/// How far the strip floats above its line, so it doesn't flicker against
/// ground flattened to the same height.
pub const ROAD_LIFT: f32 = 0.03;
/// Most rows one strip may have, however long the road.
const MAX_SAMPLES: usize = 16_384;
/// Spacing between strip rows, in world units.
const SAMPLE_SPACING: f32 = 1.0;

/// One road, saved on its object. Points are in the object's own space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoadData {
    /// The points the road passes through, in order.
    #[serde(default)]
    pub points: Vec<[f32; 3]>,
    /// Width of the flat road, in world units.
    #[serde(default = "default_width")]
    pub width: f32,
    /// How far beyond each side the ground blends back to its own height.
    #[serde(default = "default_edge")]
    pub edge: f32,
    /// Whether the last point joins back to the first.
    #[serde(default)]
    pub closed: bool,
    /// Object ID of the terrain the road is pressed into, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<String>,
    /// Terrain paint layer laid along the road (0-based), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<u32>,
    /// How strongly the paint layer covers the road, 0–1.
    #[serde(default = "default_strength")]
    pub paint_strength: f32,
    /// Whether the road has a visible surface strip.
    #[serde(default = "default_true")]
    pub surface: bool,
    /// The strip's colour (linear RGBA, 0–1), multiplied with its image.
    #[serde(default = "default_color")]
    pub color: [f32; 4],
    /// Image on the strip, repeated along it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<AssetRef>,
    /// Length of road one repeat of the image covers, in world units.
    #[serde(default = "default_tiling")]
    pub tiling: f32,
}

fn default_width() -> f32 {
    4.0
}

fn default_edge() -> f32 {
    2.0
}

fn default_strength() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_color() -> [f32; 4] {
    [0.42, 0.36, 0.30, 1.0]
}

fn default_tiling() -> f32 {
    4.0
}

impl Default for RoadData {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            width: default_width(),
            edge: default_edge(),
            closed: false,
            terrain: None,
            paint_layer: None,
            paint_strength: default_strength(),
            surface: true,
            color: default_color(),
            texture: None,
            tiling: default_tiling(),
        }
    }
}

/// One row of a sampled road line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoadSample {
    /// Point on the line, in the road's own space.
    pub position: Vec3,
    /// Unit direction of travel, flattened to the ground plane.
    pub forward: Vec3,
    /// Distance along the road from its start.
    pub distance: f32,
}

impl RoadData {
    /// Checks the points, sizes, strength and colour.
    ///
    /// # Errors
    ///
    /// A short reason when a value is out of range.
    pub fn validate(&self) -> Result<(), String> {
        if self.points.len() > MAX_ROAD_POINTS {
            return Err(format!("a road has at most {MAX_ROAD_POINTS} points"));
        }
        if !self.points.iter().flatten().all(|v| v.is_finite()) {
            return Err("every point must be a finite position".to_owned());
        }
        if !(self.width.is_finite() && self.width > 0.0 && self.width <= MAX_ROAD_WIDTH) {
            return Err("width must be above 0".to_owned());
        }
        if !(self.edge.is_finite() && (0.0..=MAX_ROAD_WIDTH).contains(&self.edge)) {
            return Err("edge must be 0 or more".to_owned());
        }
        if !(self.paint_strength.is_finite() && (0.0..=1.0).contains(&self.paint_strength)) {
            return Err("paint strength must be between 0 and 1".to_owned());
        }
        if !self
            .color
            .iter()
            .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
        {
            return Err("colour channels must be between 0 and 1".to_owned());
        }
        if !(self.tiling.is_finite() && self.tiling > 0.0) {
            return Err("tiling must be above 0".to_owned());
        }
        if self.terrain.as_deref().is_some_and(str::is_empty) {
            return Err("terrain link must not be empty".to_owned());
        }
        Ok(())
    }

    /// The points as vectors.
    fn vectors(&self) -> Vec<Vec3> {
        self.points.iter().map(|p| Vec3::from(*p)).collect()
    }

    /// The smooth line through the points, about one row per world unit
    /// (and at least four per stretch between two points). Empty with fewer
    /// than two points.
    #[must_use]
    pub fn samples(&self) -> Vec<RoadSample> {
        let points = self.vectors();
        let positions = spline(&points, self.closed, SAMPLE_SPACING);
        let mut samples = Vec::with_capacity(positions.len());
        let mut distance = 0.0;
        for (index, &position) in positions.iter().enumerate() {
            if index > 0 {
                distance += position.distance(positions[index - 1]);
            }
            let before = positions[index.saturating_sub(1)];
            let after = positions[(index + 1).min(positions.len() - 1)];
            let flat = Vec3::new(after.x - before.x, 0.0, after.z - before.z);
            let forward = flat.try_normalize().unwrap_or(Vec3::Z);
            samples.push(RoadSample {
                position,
                forward,
                distance,
            });
        }
        samples
    }

    /// The flat strip along the road, in the road's own space: vertices with
    /// up normals, the image repeating every [`RoadData::tiling`] along the
    /// road, and triangle indices facing up. `None` when the road has no
    /// surface or fewer than two points.
    #[must_use]
    pub fn ribbon(&self) -> Option<(Vec<engine_renderer::Vertex>, Vec<u32>)> {
        if !self.surface {
            return None;
        }
        let samples = self.samples();
        if samples.len() < 2 {
            return None;
        }
        let half = self.width * 0.5;
        let mut vertices = Vec::with_capacity(samples.len() * 2);
        for sample in &samples {
            let side = Vec3::new(sample.forward.z, 0.0, -sample.forward.x);
            let v = sample.distance / self.tiling;
            for (offset, u) in [(-half, 0.0), (half, 1.0)] {
                let at = sample.position + side * offset + Vec3::Y * ROAD_LIFT;
                vertices.push(engine_renderer::Vertex {
                    position: at.to_array(),
                    normal: [0.0, 1.0, 0.0],
                    uv: [u, v],
                });
            }
        }
        let rows = u32::try_from(samples.len()).ok()?;
        let mut indices = Vec::with_capacity((samples.len() - 1) * 6);
        for row in 0..rows - 1 {
            let (a, b, c, d) = (row * 2, row * 2 + 1, row * 2 + 2, row * 2 + 3);
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
        Some((vertices, indices))
    }
}

/// The live copy of an object's [`RoadData`].
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneRoad(pub RoadData);

/// Positions along a centripetal Catmull-Rom spline through `points`, about
/// `spacing` apart. A closed line ends back on its first point.
fn spline(points: &[Vec3], closed: bool, spacing: f32) -> Vec<Vec3> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let at = |index: isize| -> Vec3 {
        if closed {
            points[index.rem_euclid(n as isize) as usize]
        } else if index < 0 {
            // Mirror the first stretch so the line starts straight.
            points[0] * 2.0 - points[1]
        } else if index as usize >= n {
            points[n - 1] * 2.0 - points[n - 2]
        } else {
            points[index as usize]
        }
    };
    let stretches = if closed { n } else { n - 1 };
    let mut out = Vec::new();
    for stretch in 0..stretches {
        let i = stretch as isize;
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let steps = ((p1.distance(p2) / spacing).ceil() as usize).clamp(4, MAX_SAMPLES);
        for step in 0..steps {
            if out.len() >= MAX_SAMPLES - 1 {
                break;
            }
            out.push(catmull_rom(p0, p1, p2, p3, step as f32 / steps as f32));
        }
    }
    out.push(if closed { points[0] } else { points[n - 1] });
    out
}

/// The centripetal Catmull-Rom point a share `t` (0–1) of the way from `p1`
/// to `p2`, which never overshoots into loops at sharp turns.
fn catmull_rom(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, t: f32) -> Vec3 {
    let knot = |a: Vec3, b: Vec3| a.distance(b).sqrt().max(1e-4);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let t = t1 + (t2 - t1) * t;
    let lerp = |a: Vec3, b: Vec3, ta: f32, tb: f32| {
        a * ((tb - t) / (tb - ta)) + b * ((t - ta) / (tb - ta))
    };
    let a1 = lerp(p0, p1, t0, t1);
    let a2 = lerp(p1, p2, t1, t2);
    let a3 = lerp(p2, p3, t2, t3);
    let b1 = lerp(a1, a2, t0, t2);
    let b2 = lerp(a2, a3, t1, t3);
    lerp(b1, b2, t1, t2)
}

/// A road line ready for asking how it shapes the ground, in whatever space
/// its points were given (Studio uses the terrain's own space).
#[derive(Debug, Clone)]
pub struct RoadShape {
    points: Vec<Vec3>,
    half_width: f32,
    edge: f32,
    cell: f32,
    /// Stretch indices by the grid cells they pass near.
    cells: HashMap<(i32, i32), Vec<usize>>,
}

impl RoadShape {
    /// The shape of a line through `points` (already sampled densely), flat
    /// for `half_width` either side and blending out over `edge` more.
    #[must_use]
    pub fn new(points: Vec<Vec3>, half_width: f32, edge: f32) -> Self {
        let reach = (half_width + edge).max(1e-3);
        let cell = reach.max(1.0);
        let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (index, pair) in points.windows(2).enumerate() {
            let lo = pair[0].min(pair[1]) - Vec3::splat(reach);
            let hi = pair[0].max(pair[1]) + Vec3::splat(reach);
            let (x0, z0) = ((lo.x / cell).floor() as i32, (lo.z / cell).floor() as i32);
            let (x1, z1) = ((hi.x / cell).floor() as i32, (hi.z / cell).floor() as i32);
            for x in x0..=x1 {
                for z in z0..=z1 {
                    cells.entry((x, z)).or_default().push(index);
                }
            }
        }
        Self {
            points,
            half_width,
            edge,
            cell,
            cells,
        }
    }

    /// The ground-plane rectangle the road can touch, with its edges.
    #[must_use]
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let reach = Vec2::splat(self.half_width + self.edge);
        let mut iter = self.points.iter().map(|p| Vec2::new(p.x, p.z));
        let first = iter.next()?;
        let (lo, hi) = iter.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p)));
        Some((lo - reach, hi + reach))
    }

    /// For ground point `at`: the road's height at the nearest point of its
    /// line, and how strongly the ground there follows it (1 on the road,
    /// fading smoothly to 0 across the edge). `None` beyond the edge.
    #[must_use]
    pub fn influence(&self, at: Vec2) -> Option<(f32, f32)> {
        let key = (
            (at.x / self.cell).floor() as i32,
            (at.y / self.cell).floor() as i32,
        );
        let mut best: Option<(f32, f32)> = None;
        for &index in self.cells.get(&key)? {
            let (a, b) = (self.points[index], self.points[index + 1]);
            let (a2, b2) = (Vec2::new(a.x, a.z), Vec2::new(b.x, b.z));
            let along = b2 - a2;
            let t = if along.length_squared() > 1e-12 {
                ((at - a2).dot(along) / along.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = at.distance(a2 + along * t);
            if best.is_none_or(|(nearest, _)| distance < nearest) {
                best = Some((distance, a.y + (b.y - a.y) * t));
            }
        }
        let (distance, height) = best?;
        if distance <= self.half_width {
            return Some((height, 1.0));
        }
        if self.edge <= 0.0 || distance >= self.half_width + self.edge {
            return None;
        }
        let x = 1.0 - (distance - self.half_width) / self.edge;
        Some((height, x * x * (3.0 - 2.0 * x)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn road(points: &[[f32; 3]]) -> RoadData {
        RoadData {
            points: points.to_vec(),
            ..RoadData::default()
        }
    }

    #[test]
    fn the_line_passes_through_every_point() {
        let data = road(&[[0.0, 0.0, 0.0], [10.0, 1.0, 0.0], [10.0, 2.0, 10.0]]);
        let samples = data.samples();
        for point in &data.points {
            let point = Vec3::from(*point);
            assert!(
                samples.iter().any(|s| s.position.distance(point) < 1e-4),
                "{point} is on the line"
            );
        }
        assert_eq!(samples.first().map(|s| s.position), Some(Vec3::ZERO));
        assert!(samples.windows(2).all(|w| w[1].distance >= w[0].distance));
    }

    #[test]
    fn a_closed_road_ends_where_it_starts() {
        let mut data = road(&[[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [5.0, 0.0, 8.0]]);
        data.closed = true;
        let samples = data.samples();
        assert_eq!(samples.last().map(|s| s.position), Some(Vec3::ZERO));
    }

    #[test]
    fn the_strip_is_as_wide_as_the_road_and_faces_up() {
        let data = road(&[[0.0, 0.0, 0.0], [0.0, 0.0, 10.0]]);
        let (vertices, indices) = data.ribbon().unwrap();
        assert_eq!(vertices.len(), data.samples().len() * 2);
        assert_eq!(indices.len(), (vertices.len() / 2 - 1) * 6);
        let left = Vec3::from(vertices[0].position);
        let right = Vec3::from(vertices[1].position);
        assert!((left.distance(right) - data.width).abs() < 1e-4);
        assert!((left.y - ROAD_LIFT).abs() < 1e-6);
        for triangle in indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|i| Vec3::from(vertices[triangle[i] as usize].position));
            assert!((b - a).cross(c - a).y > 0.0, "every triangle faces up");
        }
        assert!(road(&[[0.0; 3]]).ribbon().is_none());
        let hidden = RoadData {
            surface: false,
            ..data
        };
        assert!(hidden.ribbon().is_none());
    }

    #[test]
    fn the_ground_follows_the_road_and_fades_across_the_edge() {
        let data = road(&[[0.0, 2.0, 0.0], [20.0, 4.0, 0.0]]);
        let points: Vec<Vec3> = data.samples().iter().map(|s| s.position).collect();
        let shape = RoadShape::new(points, 2.0, 2.0);
        let (height, weight) = shape.influence(Vec2::new(10.0, 1.0)).unwrap();
        assert!((height - 3.0).abs() < 0.05, "{height}");
        assert_eq!(weight, 1.0);
        let (_, fading) = shape.influence(Vec2::new(10.0, 3.0)).unwrap();
        assert!(fading > 0.0 && fading < 1.0, "{fading}");
        assert!(shape.influence(Vec2::new(10.0, 4.5)).is_none());
        assert!(shape.influence(Vec2::new(100.0, 0.0)).is_none());
        let (lo, hi) = shape.bounds().unwrap();
        assert_eq!(lo, Vec2::new(-4.0, -4.0));
        assert_eq!(hi, Vec2::new(24.0, 4.0));
    }

    #[test]
    fn validate_rejects_bad_values() {
        assert!(road(&[[0.0; 3], [1.0, 0.0, 0.0]]).validate().is_ok());
        assert!(road(&[[f32::NAN, 0.0, 0.0]]).validate().is_err());
        let wide = RoadData {
            width: 0.0,
            ..RoadData::default()
        };
        assert!(wide.validate().is_err());
        let paint = RoadData {
            paint_strength: 2.0,
            ..RoadData::default()
        };
        assert!(paint.validate().is_err());
        let too_many = road(&vec![[0.0; 3]; MAX_ROAD_POINTS + 1]);
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn older_files_load_with_defaults() {
        let data: RoadData = ron::from_str("(points: [(0, 0, 0), (1, 0, 0)])").unwrap();
        assert_eq!(data.width, 4.0);
        assert!(data.surface);
    }
}
