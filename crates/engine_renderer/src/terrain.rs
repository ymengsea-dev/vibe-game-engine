//! Heightmap terrain: an editable grid of heights, a circular-brush sculpt
//! API (raise/lower, smooth, flatten), and triangle-mesh generation from
//! it.
//!
//! All pure CPU data and math — no `wgpu`. A [`Heightmap`] is the editable
//! source of truth; [`Heightmap::mesh_data`] turns its current state into
//! the `(Vec<Vertex>, Vec<u32>)` the existing
//! [`GpuContext::create_mesh_dynamic`](crate::GpuContext::create_mesh_dynamic)
//! /
//! [`GpuContext::write_mesh_vertices`](crate::GpuContext::write_mesh_vertices)
//! path consumes, so a terrain is just a [`crate::Mesh`] drawn through the
//! normal renderer, re-uploaded after each edit.
//!
//! Layout: a `resolution x resolution` grid of vertices (row-major,
//! `heights[row * resolution + col]`), spanning `[-size/2, size/2]` on
//! local X (`col`) and Z (`row`), centred on the local origin. Height is
//! the local Y. An entity's `Transform` places the whole patch in the
//! world.
//!
//! Deliberately minimal for one iteration: a single square patch (no
//! chunking/LOD), whole-mesh regeneration per edit (no brush-local
//! partial update), no texture splatting, no collision mesh — all tracked
//! as future work.

use glam::{Vec2, Vec3};

use crate::error::RendererError;
use crate::mesh::Vertex;

/// How a [`Brush`]'s influence fades from its centre (weight `1.0`) to its
/// rim (weight `0.0`). Input is normalized distance `t = distance /
/// radius`; anything at or past `t = 1.0` has weight `0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrushFalloff {
    /// Smoothstep rolloff — soft edges, the natural-looking default.
    #[default]
    Smooth,
    /// Linear rolloff — a straight cone.
    Linear,
    /// No rolloff — full strength everywhere inside the radius, a hard
    /// cylinder.
    Constant,
}

impl BrushFalloff {
    /// This falloff's weight at normalized distance `t` (`distance /
    /// radius`). `1.0` at `t <= 0`, `0.0` at `t >= 1`, monotonically
    /// non-increasing between.
    pub fn weight(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            BrushFalloff::Smooth => {
                let x = 1.0 - t;
                x * x * (3.0 - 2.0 * x)
            }
            BrushFalloff::Linear => 1.0 - t,
            BrushFalloff::Constant => {
                if t < 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

/// A circular sculpt brush in a [`Heightmap`]'s local XZ space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brush {
    /// Brush centre, `(local_x, local_z)`.
    pub center: Vec2,
    /// Radius of influence, in local (world) units.
    pub radius: f32,
    /// Per-application strength. For [`Heightmap::raise_lower`] this is the
    /// height added at the centre (negative lowers); for
    /// [`Heightmap::smooth`] / [`Heightmap::flatten`] it's the fraction
    /// (0..=1 after weighting) of the way a cell moves toward its target.
    pub strength: f32,
    /// How influence fades toward the rim.
    pub falloff: BrushFalloff,
}

impl Brush {
    /// A brush at `center` with `radius` and `strength`, [`BrushFalloff`]
    /// default ([`BrushFalloff::Smooth`]).
    pub fn new(center: Vec2, radius: f32, strength: f32) -> Self {
        Self {
            center,
            radius,
            strength,
            falloff: BrushFalloff::Smooth,
        }
    }
}

/// An editable grid of terrain heights. See the module docs for the
/// coordinate layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Heightmap {
    resolution: u32,
    size: f32,
    heights: Vec<f32>,
}

impl Heightmap {
    /// A flat heightmap: `resolution` vertices per side over a `size` x
    /// `size` local patch, every height `0.0`.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] if `resolution < 2` or `size`
    /// isn't positive and finite.
    pub fn new(resolution: u32, size: f32) -> Result<Self, RendererError> {
        Self::validate(resolution, size)?;
        Ok(Self {
            resolution,
            size,
            heights: vec![0.0; (resolution as usize) * (resolution as usize)],
        })
    }

    /// A heightmap with explicit per-node heights, row-major
    /// (`heights[row * resolution + col]`).
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] if `resolution < 2`, `size`
    /// isn't positive and finite, or `heights.len() != resolution *
    /// resolution`.
    pub fn from_heights(
        resolution: u32,
        size: f32,
        heights: Vec<f32>,
    ) -> Result<Self, RendererError> {
        Self::validate(resolution, size)?;
        if heights.len() != (resolution as usize) * (resolution as usize) {
            return Err(RendererError::InvalidHeightmap {
                reason: "heights length must equal resolution squared",
            });
        }
        Ok(Self {
            resolution,
            size,
            heights,
        })
    }

    /// A heightmap whose node heights come from `height_fn(row, col)`.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] if `resolution < 2` or `size`
    /// isn't positive and finite.
    pub fn from_fn(
        resolution: u32,
        size: f32,
        height_fn: impl Fn(u32, u32) -> f32,
    ) -> Result<Self, RendererError> {
        Self::validate(resolution, size)?;
        let mut heights = Vec::with_capacity((resolution as usize) * (resolution as usize));
        for row in 0..resolution {
            for col in 0..resolution {
                heights.push(height_fn(row, col));
            }
        }
        Ok(Self {
            resolution,
            size,
            heights,
        })
    }

    fn validate(resolution: u32, size: f32) -> Result<(), RendererError> {
        if resolution < 2 {
            return Err(RendererError::InvalidHeightmap {
                reason: "resolution must be at least 2",
            });
        }
        if size <= 0.0 || !size.is_finite() {
            return Err(RendererError::InvalidHeightmap {
                reason: "size must be positive and finite",
            });
        }
        Ok(())
    }

    /// Vertices per side.
    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Local-space edge length of the (square) patch.
    pub fn size(&self) -> f32 {
        self.size
    }

    /// The raw row-major height grid.
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// Local-space distance between adjacent grid nodes.
    pub fn cell_size(&self) -> f32 {
        self.size / (self.resolution - 1) as f32
    }

    /// The height at node `(row, col)`, with `row`/`col` clamped into
    /// `0..resolution` (so edge/corner lookups for normals and smoothing
    /// never go out of bounds).
    pub fn node_height(&self, row: i64, col: i64) -> f32 {
        let last = (self.resolution - 1) as i64;
        let row = row.clamp(0, last) as usize;
        let col = col.clamp(0, last) as usize;
        self.heights[row * self.resolution as usize + col]
    }

    /// Bilinearly-interpolated height at local position `(local_x,
    /// local_z)`. Positions outside the patch clamp to the nearest edge.
    pub fn height_at(&self, local_x: f32, local_z: f32) -> f32 {
        let last = (self.resolution - 1) as f32;
        let half = self.size * 0.5;
        let cell = self.cell_size();

        let fx = ((local_x + half) / cell).clamp(0.0, last);
        let fz = ((local_z + half) / cell).clamp(0.0, last);

        let col0 = fx.floor();
        let row0 = fz.floor();
        let tx = fx - col0;
        let tz = fz - row0;
        let (col0, row0) = (col0 as i64, row0 as i64);

        let h00 = self.node_height(row0, col0);
        let h10 = self.node_height(row0, col0 + 1);
        let h01 = self.node_height(row0 + 1, col0);
        let h11 = self.node_height(row0 + 1, col0 + 1);

        let top = h00 + (h10 - h00) * tx;
        let bottom = h01 + (h11 - h01) * tx;
        top + (bottom - top) * tz
    }

    /// The grid-index rectangle `[col_min, col_max] x [row_min, row_max]`
    /// (inclusive, clamped to the grid) whose nodes could fall inside
    /// `brush`. Empty (`col_min > col_max`) if the brush misses the patch.
    fn affected_rect(&self, brush: &Brush) -> (i64, i64, i64, i64) {
        let last = (self.resolution - 1) as i64;
        let half = self.size * 0.5;
        let cell = self.cell_size();
        let to_index = |world: f32| ((world + half) / cell).floor() as i64;

        let col_min = to_index(brush.center.x - brush.radius).clamp(0, last);
        let col_max = (to_index(brush.center.x + brush.radius) + 1).clamp(0, last);
        let row_min = to_index(brush.center.y - brush.radius).clamp(0, last);
        let row_max = (to_index(brush.center.y + brush.radius) + 1).clamp(0, last);
        (col_min, col_max, row_min, row_max)
    }

    /// Local-space `(x, z)` of node `(row, col)`.
    fn node_position_xz(&self, row: i64, col: i64) -> Vec2 {
        let half = self.size * 0.5;
        let cell = self.cell_size();
        Vec2::new(-half + col as f32 * cell, -half + row as f32 * cell)
    }

    /// Unit surface normal at node `(row, col)`, from the central-difference
    /// height gradient (one-sided at the grid edges). `+Y` where the
    /// surface is flat or the gradient degenerates.
    fn node_normal(&self, row: i64, col: i64) -> Vec3 {
        let last = self.resolution as i64 - 1;
        let cell = self.cell_size();
        let (cl, cr) = ((col - 1).max(0), (col + 1).min(last));
        let (rd, ru) = ((row - 1).max(0), (row + 1).min(last));
        let dhdx =
            (self.node_height(row, cr) - self.node_height(row, cl)) / ((cr - cl) as f32 * cell);
        let dhdz =
            (self.node_height(ru, col) - self.node_height(rd, col)) / ((ru - rd) as f32 * cell);
        let normal = Vec3::new(-dhdx, 1.0, -dhdz).normalize_or_zero();
        if normal == Vec3::ZERO {
            Vec3::Y
        } else {
            normal
        }
    }

    /// Bilinearly-blended unit surface normal at local position
    /// `(local_x, local_z)` — the interpolated counterpart of
    /// [`Heightmap::height_at`], using the same four surrounding nodes.
    /// Positions outside the patch clamp to the nearest edge.
    pub fn normal_at(&self, local_x: f32, local_z: f32) -> Vec3 {
        let last = (self.resolution - 1) as f32;
        let half = self.size * 0.5;
        let cell = self.cell_size();

        let fx = ((local_x + half) / cell).clamp(0.0, last);
        let fz = ((local_z + half) / cell).clamp(0.0, last);
        let col0 = fx.floor();
        let row0 = fz.floor();
        let tx = fx - col0;
        let tz = fz - row0;
        let (col0, row0) = (col0 as i64, row0 as i64);

        let n00 = self.node_normal(row0, col0);
        let n10 = self.node_normal(row0, col0 + 1);
        let n01 = self.node_normal(row0 + 1, col0);
        let n11 = self.node_normal(row0 + 1, col0 + 1);

        let top = n00.lerp(n10, tx);
        let bottom = n01.lerp(n11, tx);
        let blended = top.lerp(bottom, tz).normalize_or_zero();
        if blended == Vec3::ZERO {
            Vec3::Y
        } else {
            blended
        }
    }

    /// Terrain steepness at local position `(local_x, local_z)`: the angle
    /// between [`Heightmap::normal_at`] and straight up, in radians —
    /// `0.0` on flat ground, up to `PI / 2` on a vertical face.
    pub fn slope_at(&self, local_x: f32, local_z: f32) -> f32 {
        self.normal_at(local_x, local_z).y.clamp(-1.0, 1.0).acos()
    }

    /// Runs `apply(row, col, weight)` for every grid node inside `brush`,
    /// where `weight` is `brush.strength * falloff` (already includes the
    /// brush strength).
    fn for_each_affected(&self, brush: &Brush, mut apply: impl FnMut(usize, usize, f32)) {
        if brush.radius <= 0.0 || !brush.radius.is_finite() || !brush.strength.is_finite() {
            return;
        }
        let (col_min, col_max, row_min, row_max) = self.affected_rect(brush);
        for row in row_min..=row_max {
            for col in col_min..=col_max {
                let d = self.node_position_xz(row, col).distance(brush.center);
                if d > brush.radius {
                    continue;
                }
                let weight = brush.falloff.weight(d / brush.radius) * brush.strength;
                if weight != 0.0 {
                    apply(row as usize, col as usize, weight);
                }
            }
        }
    }

    fn index(&self, row: usize, col: usize) -> usize {
        row * self.resolution as usize + col
    }

    /// Adds `brush.strength * falloff` to every height inside `brush`
    /// (negative strength lowers). No-op for a non-positive/non-finite
    /// radius or non-finite strength.
    pub fn raise_lower(&mut self, brush: &Brush) {
        let mut deltas: Vec<(usize, f32)> = Vec::new();
        self.for_each_affected(brush, |row, col, weight| {
            deltas.push((self.index(row, col), weight));
        });
        for (index, delta) in deltas {
            self.heights[index] += delta;
        }
    }

    /// Moves every height inside `brush` toward its 4-neighbour average by
    /// `(brush.strength * falloff).clamp(0, 1)`. Neighbour heights are read
    /// from a pre-edit snapshot, so the result doesn't depend on
    /// traversal order.
    pub fn smooth(&mut self, brush: &Brush) {
        let snapshot = self.heights.clone();
        let resolution = self.resolution as i64;
        let read = |row: i64, col: i64| {
            let last = resolution - 1;
            let row = row.clamp(0, last) as usize;
            let col = col.clamp(0, last) as usize;
            snapshot[row * self.resolution as usize + col]
        };

        let mut updates: Vec<(usize, f32)> = Vec::new();
        self.for_each_affected(brush, |row, col, weight| {
            let r = row as i64;
            let c = col as i64;
            let average =
                (read(r - 1, c) + read(r + 1, c) + read(r, c - 1) + read(r, c + 1)) * 0.25;
            let current = snapshot[row * self.resolution as usize + col];
            let k = weight.clamp(0.0, 1.0);
            updates.push((self.index(row, col), current + (average - current) * k));
        });
        for (index, value) in updates {
            self.heights[index] = value;
        }
    }

    /// Moves every height inside `brush` toward `target` by
    /// `(brush.strength * falloff).clamp(0, 1)` — so `strength >= 1` with
    /// [`BrushFalloff::Constant`] sets the whole disc exactly to `target`.
    pub fn flatten(&mut self, brush: &Brush, target: f32) {
        let mut updates: Vec<(usize, f32)> = Vec::new();
        self.for_each_affected(brush, |row, col, weight| {
            let index = self.index(row, col);
            let current = self.heights[index];
            let k = weight.clamp(0.0, 1.0);
            updates.push((index, current + (target - current) * k));
        });
        for (index, value) in updates {
            self.heights[index] = value;
        }
    }

    /// This heightmap's current state as renderer geometry: one [`Vertex`]
    /// per grid node (local position, a normal from neighbouring heights,
    /// and a `[0, 1]` UV across the patch) and a triangle-list index
    /// buffer (`(resolution - 1)^2 * 6` indices, CCW when viewed from
    /// `+Y`).
    ///
    /// The index buffer only depends on `resolution`, so a re-sculpt needs
    /// only the vertices ([`Heightmap::vertices`]) re-uploaded.
    pub fn mesh_data(&self) -> (Vec<Vertex>, Vec<u32>) {
        (self.vertices(), self.indices())
    }

    /// Just the vertices of [`Heightmap::mesh_data`] — what
    /// [`GpuContext::write_mesh_vertices`](crate::GpuContext::write_mesh_vertices)
    /// needs after a sculpt (the index buffer is unchanged).
    pub fn vertices(&self) -> Vec<Vertex> {
        let resolution = self.resolution as i64;
        let last = (self.resolution - 1) as f32;
        let mut vertices = Vec::with_capacity((self.resolution as usize).pow(2));

        for row in 0..resolution {
            for col in 0..resolution {
                let xz = self.node_position_xz(row, col);
                let height = self.node_height(row, col);
                let normal = self.node_normal(row, col);

                vertices.push(Vertex {
                    position: [xz.x, height, xz.y],
                    normal: normal.to_array(),
                    uv: [col as f32 / last, row as f32 / last],
                });
            }
        }
        vertices
    }

    /// Just the indices of [`Heightmap::mesh_data`]. Depends only on
    /// `resolution`, so it's built once and reused across re-sculpts.
    pub fn indices(&self) -> Vec<u32> {
        let res = self.resolution;
        let mut indices = Vec::with_capacity(((res - 1) as usize).pow(2) * 6);
        for row in 0..res - 1 {
            for col in 0..res - 1 {
                let tl = row * res + col;
                let tr = tl + 1;
                let bl = (row + 1) * res + col;
                let br = bl + 1;
                // Two CCW triangles seen from +Y, matching `cube`'s +Y
                // face winding.
                indices.extend_from_slice(&[tl, tr, br, tl, br, bl]);
            }
        }
        indices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_tiny_resolution_and_bad_size() {
        assert!(matches!(
            Heightmap::new(1, 10.0),
            Err(RendererError::InvalidHeightmap { .. })
        ));
        assert!(Heightmap::new(4, 0.0).is_err());
        assert!(Heightmap::new(4, -1.0).is_err());
        assert!(Heightmap::new(4, f32::INFINITY).is_err());
        assert!(Heightmap::new(2, 1.0).is_ok());
    }

    #[test]
    fn from_heights_checks_length() {
        assert!(Heightmap::from_heights(3, 1.0, vec![0.0; 9]).is_ok());
        assert!(matches!(
            Heightmap::from_heights(3, 1.0, vec![0.0; 8]),
            Err(RendererError::InvalidHeightmap { .. })
        ));
    }

    #[test]
    fn from_fn_fills_row_major() {
        let hm = Heightmap::from_fn(3, 2.0, |row, col| (row * 10 + col) as f32).unwrap();
        assert_eq!(
            hm.heights(),
            &[0.0, 1.0, 2.0, 10.0, 11.0, 12.0, 20.0, 21.0, 22.0]
        );
    }

    #[test]
    fn flat_heightmap_reads_zero_everywhere() {
        let hm = Heightmap::new(8, 16.0).unwrap();
        assert_eq!(hm.height_at(0.0, 0.0), 0.0);
        assert_eq!(hm.height_at(-100.0, 100.0), 0.0);
        assert_eq!(hm.height_at(3.3, -1.7), 0.0);
    }

    #[test]
    fn height_at_is_exact_on_grid_nodes() {
        // 2x2 grid over a 2-unit patch: nodes at local -1 and +1.
        let hm = Heightmap::from_heights(2, 2.0, vec![0.0, 1.0, 2.0, 3.0]).unwrap();
        assert_eq!(hm.height_at(-1.0, -1.0), 0.0); // row 0 col 0
        assert_eq!(hm.height_at(1.0, -1.0), 1.0); // row 0 col 1
        assert_eq!(hm.height_at(-1.0, 1.0), 2.0); // row 1 col 0
        assert_eq!(hm.height_at(1.0, 1.0), 3.0); // row 1 col 1
    }

    #[test]
    fn height_at_bilinearly_interpolates_and_clamps() {
        let hm = Heightmap::from_heights(2, 2.0, vec![0.0, 1.0, 2.0, 3.0]).unwrap();
        // Centre of the single cell: average of the four corners = 1.5.
        assert!((hm.height_at(0.0, 0.0) - 1.5).abs() < 1e-6);
        // Outside the patch clamps to the nearest edge node.
        assert_eq!(hm.height_at(-99.0, -99.0), 0.0);
        assert_eq!(hm.height_at(99.0, 99.0), 3.0);
    }

    #[test]
    fn raise_lower_peaks_at_the_centre_and_stops_at_the_radius() {
        let mut hm = Heightmap::new(21, 20.0).unwrap();
        let brush = Brush::new(Vec2::ZERO, 5.0, 2.0);
        hm.raise_lower(&brush);

        let centre = hm.height_at(0.0, 0.0);
        let mid = hm.height_at(2.5, 0.0);
        let edge = hm.height_at(4.8, 0.0);
        let outside = hm.height_at(8.0, 0.0);

        assert!((centre - 2.0).abs() < 1e-4, "centre {centre}");
        assert!(mid > 0.0 && mid < centre);
        assert!(edge >= 0.0 && edge < mid);
        assert_eq!(outside, 0.0);
    }

    #[test]
    fn raise_lower_negative_strength_digs_down() {
        let mut hm = Heightmap::new(21, 20.0).unwrap();
        hm.raise_lower(&Brush::new(Vec2::ZERO, 5.0, -3.0));
        assert!((hm.height_at(0.0, 0.0) + 3.0).abs() < 1e-4);
    }

    #[test]
    fn raise_lower_at_a_corner_touches_no_out_of_bounds_cell() {
        let mut hm = Heightmap::new(9, 8.0).unwrap();
        // Centre on the far corner node.
        hm.raise_lower(&Brush::new(Vec2::new(4.0, 4.0), 3.0, 1.0));
        assert!(hm.height_at(4.0, 4.0) > 0.0);
        // No panic, and the opposite corner is untouched.
        assert_eq!(hm.height_at(-4.0, -4.0), 0.0);
    }

    #[test]
    fn constant_falloff_full_strength_raise_is_uniform_inside_the_disc() {
        let mut hm = Heightmap::new(21, 20.0).unwrap();
        let brush = Brush {
            center: Vec2::ZERO,
            radius: 4.0,
            strength: 1.0,
            falloff: BrushFalloff::Constant,
        };
        hm.raise_lower(&brush);
        assert!((hm.height_at(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((hm.height_at(3.0, 0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn smooth_reduces_a_spike_and_leaves_a_flat_patch_alone() {
        let resolution = 11;
        let mut spike = vec![0.0f32; resolution * resolution];
        let centre = (resolution / 2) * resolution + resolution / 2;
        spike[centre] = 10.0;
        let mut hm = Heightmap::from_heights(resolution as u32, 10.0, spike).unwrap();

        let before = hm.heights()[centre];
        // Half-strength: the spike is pulled toward its (zero) neighbours
        // but not all the way, so it stays positive.
        hm.smooth(&Brush {
            center: Vec2::ZERO,
            radius: 3.0,
            strength: 0.5,
            falloff: BrushFalloff::Constant,
        });
        let after = hm.heights()[centre];
        assert!(after < before && after > 0.0, "{before} -> {after}");

        // A genuinely flat map is unchanged by smoothing.
        let mut flat = Heightmap::new(11, 10.0).unwrap();
        let flat_before = flat.clone();
        flat.smooth(&Brush::new(Vec2::ZERO, 4.0, 1.0));
        assert_eq!(flat, flat_before);
    }

    #[test]
    fn smooth_is_traversal_order_independent() {
        // Two heightmaps with the same content; smoothing reads a
        // snapshot, so the outcome can't depend on cell order.
        let data: Vec<f32> = (0..49).map(|i| (i % 7) as f32).collect();
        let mut a = Heightmap::from_heights(7, 7.0, data.clone()).unwrap();
        let mut b = Heightmap::from_heights(7, 7.0, data).unwrap();
        let brush = Brush::new(Vec2::ZERO, 3.5, 0.7);
        a.smooth(&brush);
        b.smooth(&brush);
        assert_eq!(a, b);
    }

    #[test]
    fn flatten_moves_cells_toward_the_target() {
        let mut hm = Heightmap::from_heights(11, 10.0, vec![5.0; 121]).unwrap();
        hm.flatten(
            &Brush {
                center: Vec2::ZERO,
                radius: 3.0,
                strength: 1.0,
                falloff: BrushFalloff::Constant,
            },
            0.0,
        );
        assert!((hm.height_at(0.0, 0.0) - 0.0).abs() < 1e-6);
        // Outside the brush: untouched.
        assert_eq!(hm.height_at(4.5, 0.0), 5.0);
    }

    #[test]
    fn falloff_weights_span_one_to_zero_monotonically() {
        for falloff in [
            BrushFalloff::Smooth,
            BrushFalloff::Linear,
            BrushFalloff::Constant,
        ] {
            assert_eq!(falloff.weight(0.0), 1.0);
            assert_eq!(falloff.weight(1.0), 0.0);
            assert_eq!(falloff.weight(2.0), 0.0);
            let quarter = falloff.weight(0.25);
            let three_q = falloff.weight(0.75);
            assert!(quarter >= three_q);
            assert!((0.0..=1.0).contains(&quarter));
        }
    }

    #[test]
    fn mesh_data_has_the_expected_counts_and_in_bounds_indices() {
        let hm = Heightmap::new(9, 8.0).unwrap();
        let (vertices, indices) = hm.mesh_data();
        assert_eq!(vertices.len(), 81);
        assert_eq!(indices.len(), 8 * 8 * 6);
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
    }

    #[test]
    fn flat_terrain_mesh_points_straight_up_with_corner_uvs() {
        let hm = Heightmap::new(5, 4.0).unwrap();
        let vertices = hm.vertices();
        for v in &vertices {
            assert!((v.normal[1] - 1.0).abs() < 1e-6);
            assert!(v.normal[0].abs() < 1e-6 && v.normal[2].abs() < 1e-6);
            assert_eq!(v.position[1], 0.0);
        }
        assert_eq!(vertices[0].uv, [0.0, 0.0]);
        assert_eq!(vertices[vertices.len() - 1].uv, [1.0, 1.0]);
    }

    #[test]
    fn normal_and_slope_on_flat_ground_point_straight_up() {
        let hm = Heightmap::new(6, 10.0).unwrap();
        let normal = hm.normal_at(1.3, -2.7);
        assert!((normal - Vec3::Y).length() < 1e-5);
        assert!(hm.slope_at(1.3, -2.7).abs() < 1e-5);
    }

    #[test]
    fn slope_tracks_a_ramp_and_matches_the_mesh_normals() {
        // A plane tilted along +X: height = x, so the surface makes a
        // 45-degree angle with the ground.
        let hm = Heightmap::from_fn(9, 8.0, |_, col| col as f32 * (8.0 / 8.0)).unwrap();
        let slope = hm.slope_at(0.0, 0.0);
        assert!(
            (slope - std::f32::consts::FRAC_PI_4).abs() < 0.05,
            "slope {slope} not ~45 degrees"
        );
        // Interior mesh normals agree with `normal_at` at their node.
        let node_normal = hm.normal_at(hm.size() * -0.5 + hm.cell_size() * 4.0, 0.0);
        assert!(node_normal.y > 0.0 && node_normal.x < 0.0);
    }

    #[test]
    fn normal_at_clamps_outside_the_patch() {
        let hm = Heightmap::from_fn(5, 4.0, |_, col| col as f32).unwrap();
        // Far outside on every side — no panic, always a unit-ish vector.
        for (x, z) in [(-100.0, 0.0), (100.0, 0.0), (0.0, -100.0), (0.0, 100.0)] {
            let n = hm.normal_at(x, z);
            assert!((n.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn terrain_vertex_positions_carry_the_heights() {
        let hm = Heightmap::from_fn(4, 6.0, |row, col| (row + col) as f32).unwrap();
        let vertices = hm.vertices();
        for row in 0..4u32 {
            for col in 0..4u32 {
                let v = vertices[(row * 4 + col) as usize];
                assert_eq!(v.position[1], (row + col) as f32);
            }
        }
    }

    #[test]
    fn terrain_triangles_wind_like_the_cube_top_face() {
        // The terrain reuses cube()'s +Y-face corner order and index
        // pattern (a,b,c / a,c,d over (-x,-z),(+x,-z),(+x,+z),(-x,+z)),
        // which ships correct under wgpu's default `FrontFace::Ccw`.
        // Screen-space winding isn't the world-space cross-product sign
        // (the view transform flips handedness), so compare orientation
        // against the known-good cube face rather than asserting a sign.
        let (cube_vertices, cube_indices) = crate::cube();
        let cp = |i: u32| Vec3::from(cube_vertices[i as usize].position);
        // faces = [+X, -X, +Y, -Y, +Z, -Z]; the +Y face's first triangle
        // is indices 12..15.
        let cube_sign = (cp(cube_indices[13]) - cp(cube_indices[12]))
            .cross(cp(cube_indices[14]) - cp(cube_indices[12]))
            .y
            .signum();

        let hm = Heightmap::new(3, 2.0).unwrap();
        let (vertices, indices) = hm.mesh_data();
        let tp = |i: u32| Vec3::from(vertices[i as usize].position);
        let terrain_sign = (tp(indices[1]) - tp(indices[0]))
            .cross(tp(indices[2]) - tp(indices[0]))
            .y
            .signum();

        assert_eq!(terrain_sign, cube_sign);
    }

    #[test]
    fn sculpted_terrain_normals_stay_unit_length_and_upward() {
        let mut hm = Heightmap::new(21, 20.0).unwrap();
        hm.raise_lower(&Brush::new(Vec2::ZERO, 6.0, 4.0));
        let vertices = hm.vertices();
        for v in &vertices {
            let n = Vec3::from(v.normal);
            assert!(n.y > 0.0, "normal points down or sideways: {n:?}");
            assert!((n.length() - 1.0).abs() < 1e-4);
        }
        // The bump actually tilted some normals off vertical.
        assert!(
            vertices
                .iter()
                .any(|v| v.normal[0].abs() > 1e-3 || v.normal[2].abs() > 1e-3)
        );
    }
}
