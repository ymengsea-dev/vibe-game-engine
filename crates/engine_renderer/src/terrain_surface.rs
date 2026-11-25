//! A terrain made of square tiles side by side, each a [`Heightmap`] plus
//! paint weights ([`SplatMap`]).
//!
//! Every tile has the same resolution and size. Tile `(x, z)` is centred at
//! `(x * size, z * size)` in terrain-local space, so tile `(0, 0)` covers the
//! same `[-size/2, size/2]` patch a lone [`Heightmap`] does.
//!
//! Tiles share their edge nodes: global node column `g` is column
//! `g mod (resolution - 1)` of tile `g div (resolution - 1)`, and also the
//! last column of the tile before it when it sits on the boundary. Every
//! write goes to all copies of a node, so neighbouring tiles always agree
//! along their edges and no stitching pass is needed. Brushes, normals and
//! smoothing read across tile edges the same way.
//!
//! Pure CPU math, no `wgpu`.

use std::collections::BTreeMap;

use glam::{Vec2, Vec3};

use crate::error::RendererError;
use crate::terrain::{Brush, Heightmap};

/// Most tiles one terrain can hold.
pub const MAX_TERRAIN_TILES: usize = 64;
/// Most height samples one terrain can hold, counted over the rectangle its
/// tiles span (that is what the merged collider allocates).
pub const MAX_TERRAIN_SAMPLES: usize = 4_500_000;
/// Most paint layers one terrain can blend.
pub const MAX_TERRAIN_LAYERS: usize = 8;

/// A tile's grid position: `(x, z)`.
pub type TileCoord = (i32, i32);

/// Paint weights: one texel per height node, eight layer weights per texel
/// that add up to 255.
#[derive(Debug, Clone, PartialEq)]
pub struct SplatMap {
    resolution: u32,
    weights: Vec<[u8; MAX_TERRAIN_LAYERS]>,
}

impl SplatMap {
    /// A map painted entirely with layer 0.
    pub fn new(resolution: u32) -> Self {
        let mut texel = [0; MAX_TERRAIN_LAYERS];
        texel[0] = 255;
        Self {
            resolution,
            weights: vec![texel; (resolution as usize).pow(2)],
        }
    }

    /// A map from RGBA8 pixels: `first` holds layers 0 to 3, `second` (if
    /// any) layers 4 to 7. Texels are rescaled to add up to 255; an all-zero
    /// texel becomes layer 0.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] if a buffer isn't
    /// `resolution * resolution * 4` bytes long.
    pub fn from_rgba(
        resolution: u32,
        first: &[u8],
        second: Option<&[u8]>,
    ) -> Result<Self, RendererError> {
        let texels = (resolution as usize).pow(2);
        let wrong = |bytes: &[u8]| bytes.len() != texels * 4;
        if wrong(first) || second.is_some_and(wrong) {
            return Err(RendererError::InvalidHeightmap {
                reason: "paint map size must match the terrain resolution",
            });
        }
        let weights = (0..texels)
            .map(|index| {
                let mut texel = [0; MAX_TERRAIN_LAYERS];
                texel[..4].copy_from_slice(&first[index * 4..index * 4 + 4]);
                if let Some(second) = second {
                    texel[4..].copy_from_slice(&second[index * 4..index * 4 + 4]);
                }
                normalize_texel(texel)
            })
            .collect();
        Ok(Self {
            resolution,
            weights,
        })
    }

    /// Texels per side (the same as the tile's height resolution).
    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Row-major layer weights.
    pub fn weights(&self) -> &[[u8; MAX_TERRAIN_LAYERS]] {
        &self.weights
    }

    /// Layers `4 * map .. 4 * map + 4` as tightly packed RGBA8 rows.
    pub fn rgba(&self, map: usize) -> Vec<u8> {
        let start = (map * 4).min(MAX_TERRAIN_LAYERS - 4);
        self.weights
            .iter()
            .flat_map(|texel| {
                [
                    texel[start],
                    texel[start + 1],
                    texel[start + 2],
                    texel[start + 3],
                ]
            })
            .collect()
    }

    /// Whether any texel uses layers 4 to 7, so a second map must be saved.
    pub fn uses_second_map(&self) -> bool {
        self.weights
            .iter()
            .any(|texel| texel[4..].iter().any(|w| *w != 0))
    }

    fn texel(&self, row: usize, col: usize) -> [u8; MAX_TERRAIN_LAYERS] {
        self.weights[row * self.resolution as usize + col]
    }

    fn set_texel(&mut self, row: usize, col: usize, texel: [u8; MAX_TERRAIN_LAYERS]) {
        let resolution = self.resolution as usize;
        if row < resolution && col < resolution {
            self.weights[row * resolution + col] = texel;
        }
    }
}

/// Rescales `texel` so its weights add up to exactly 255.
fn normalize_texel(texel: [u8; MAX_TERRAIN_LAYERS]) -> [u8; MAX_TERRAIN_LAYERS] {
    let sum: u32 = texel.iter().map(|w| u32::from(*w)).sum();
    if sum == 255 {
        return texel;
    }
    let mut out = [0u8; MAX_TERRAIN_LAYERS];
    if sum == 0 {
        out[0] = 255;
        return out;
    }
    let scale = 255.0 / sum as f32;
    for (slot, weight) in out.iter_mut().zip(texel) {
        *slot = (f32::from(weight) * scale).round().clamp(0.0, 255.0) as u8;
    }
    let largest = (0..MAX_TERRAIN_LAYERS)
        .max_by_key(|index| out[*index])
        .unwrap_or(0);
    fix_sum(&mut out, largest);
    out
}

/// Puts the rounding error of `texel` on layer `keep` so the sum is 255.
fn fix_sum(texel: &mut [u8; MAX_TERRAIN_LAYERS], keep: usize) {
    let others: i32 = (0..MAX_TERRAIN_LAYERS)
        .filter(|index| *index != keep)
        .map(|index| i32::from(texel[index]))
        .sum();
    texel[keep] = (255 - others).clamp(0, 255) as u8;
}

/// Moves `texel` toward pure `layer` by `amount` (0 to 1).
fn blend_toward(
    texel: [u8; MAX_TERRAIN_LAYERS],
    layer: usize,
    amount: f32,
) -> [u8; MAX_TERRAIN_LAYERS] {
    let texel = normalize_texel(texel);
    let amount = amount.clamp(0.0, 1.0);
    let mut out = [0u8; MAX_TERRAIN_LAYERS];
    for (index, slot) in out.iter_mut().enumerate() {
        let current = f32::from(texel[index]);
        let target = if index == layer { 255.0 } else { 0.0 };
        *slot = (current + (target - current) * amount)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    fix_sum(&mut out, layer);
    out
}

/// One tile: heights and paint.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainTile {
    /// Height nodes.
    pub heights: Heightmap,
    /// Paint weights, one texel per height node.
    pub splat: SplatMap,
}

impl TerrainTile {
    /// A flat tile painted with layer 0.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] for a bad resolution or size.
    pub fn flat(resolution: u32, size: f32) -> Result<Self, RendererError> {
        Ok(Self {
            heights: Heightmap::new(resolution, size)?,
            splat: SplatMap::new(resolution),
        })
    }
}

/// An inclusive rectangle of global height nodes an edit changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    /// First changed global node column.
    pub col_min: i64,
    /// Last changed global node column.
    pub col_max: i64,
    /// First changed global node row.
    pub row_min: i64,
    /// Last changed global node row.
    pub row_max: i64,
}

impl DirtyRect {
    fn point(row: i64, col: i64) -> Self {
        Self {
            col_min: col,
            col_max: col,
            row_min: row,
            row_max: row,
        }
    }

    /// The smallest rectangle holding both.
    pub fn union(self, other: Self) -> Self {
        Self {
            col_min: self.col_min.min(other.col_min),
            col_max: self.col_max.max(other.col_max),
            row_min: self.row_min.min(other.row_min),
            row_max: self.row_max.max(other.row_max),
        }
    }

    /// This rectangle grown by `nodes` on every side.
    pub fn grow(self, nodes: i64) -> Self {
        Self {
            col_min: self.col_min - nodes,
            col_max: self.col_max + nodes,
            row_min: self.row_min - nodes,
            row_max: self.row_max + nodes,
        }
    }

    /// Whether global node `(row, col)` is inside.
    pub fn contains(&self, row: i64, col: i64) -> bool {
        (self.row_min..=self.row_max).contains(&row) && (self.col_min..=self.col_max).contains(&col)
    }
}

/// Merges two optional rectangles.
pub fn union_dirty(a: Option<DirtyRect>, b: Option<DirtyRect>) -> Option<DirtyRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Every tile's heights merged into one grid over the rectangle the tiles
/// span: what the heightfield collider is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedGrid {
    /// Node rows (along Z).
    pub rows: usize,
    /// Node columns (along X).
    pub cols: usize,
    /// Row-major heights; nodes outside every tile are `0`.
    pub heights: Vec<f32>,
    /// Row-major, `(rows - 1) * (cols - 1)`: whether a tile covers the cell.
    pub cell_present: Vec<bool>,
    /// Terrain-local XZ of node `(0, 0)`.
    pub min: Vec2,
    /// Distance between neighbouring nodes.
    pub cell: f32,
}

impl MergedGrid {
    /// Terrain-local XZ of the grid's centre.
    pub fn center(&self) -> Vec2 {
        self.min
            + Vec2::new(
                (self.cols - 1) as f32 * self.cell,
                (self.rows - 1) as f32 * self.cell,
            ) * 0.5
    }
}

/// Tiles of heights and paint. See the module docs for the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainSurface {
    resolution: u32,
    size: f32,
    tiles: BTreeMap<TileCoord, TerrainTile>,
}

impl TerrainSurface {
    /// An empty terrain whose tiles will have `resolution` nodes per side
    /// over `size` units.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] for a bad resolution or size.
    pub fn new(resolution: u32, size: f32) -> Result<Self, RendererError> {
        Heightmap::validate(resolution, size)?;
        Ok(Self {
            resolution,
            size,
            tiles: BTreeMap::new(),
        })
    }

    /// Nodes per tile side.
    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Tile edge length.
    pub fn size(&self) -> f32 {
        self.size
    }

    /// Distance between neighbouring nodes.
    pub fn cell_size(&self) -> f32 {
        self.size / (self.resolution - 1) as f32
    }

    fn cells_per_tile(&self) -> i64 {
        i64::from(self.resolution) - 1
    }

    /// The tiles, in coordinate order.
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &TerrainTile)> {
        self.tiles.iter()
    }

    /// Number of tiles.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// One tile.
    pub fn tile(&self, coord: TileCoord) -> Option<&TerrainTile> {
        self.tiles.get(&coord)
    }

    /// Terrain-local XZ of a tile's centre.
    pub fn tile_center(&self, coord: TileCoord) -> Vec2 {
        Vec2::new(coord.0 as f32, coord.1 as f32) * self.size
    }

    /// Global node `(row, col)` of a tile's local node `(0, 0)`.
    pub fn tile_origin(&self, coord: TileCoord) -> (i64, i64) {
        let n = self.cells_per_tile();
        (i64::from(coord.1) * n, i64::from(coord.0) * n)
    }

    /// Adds a tile. Nodes it shares with tiles already present take their
    /// values, so the edges agree.
    ///
    /// # Errors
    ///
    /// [`RendererError::InvalidHeightmap`] if the tile's resolution or size
    /// differs from the terrain's, or it would exceed [`MAX_TERRAIN_TILES`]
    /// or [`MAX_TERRAIN_SAMPLES`].
    pub fn insert_tile(
        &mut self,
        coord: TileCoord,
        tile: TerrainTile,
    ) -> Result<(), RendererError> {
        if tile.heights.resolution() != self.resolution
            || tile.splat.resolution() != self.resolution
            || tile.heights.size() != self.size
        {
            return Err(RendererError::InvalidHeightmap {
                reason: "every terrain tile must have the same resolution and size",
            });
        }
        if !self.tiles.contains_key(&coord) && self.tiles.len() >= MAX_TERRAIN_TILES {
            return Err(RendererError::InvalidHeightmap {
                reason: "a terrain can have at most 64 tiles",
            });
        }
        let (min, max) = self.coord_bounds().map_or((coord, coord), |(min, max)| {
            (
                (min.0.min(coord.0), min.1.min(coord.1)),
                (max.0.max(coord.0), max.1.max(coord.1)),
            )
        });
        let n = self.cells_per_tile() as usize;
        let cols = (max.0 - min.0 + 1) as usize * n + 1;
        let rows = (max.1 - min.1 + 1) as usize * n + 1;
        if rows.saturating_mul(cols) > MAX_TERRAIN_SAMPLES {
            return Err(RendererError::InvalidHeightmap {
                reason: "the terrain's tiles span more than 4.5 million height samples",
            });
        }
        let mut tile = tile;
        let (row0, col0) = self.tile_origin(coord);
        let last = n;
        for local_row in 0..=last {
            for local_col in 0..=last {
                if local_row != 0 && local_row != last && local_col != 0 && local_col != last {
                    continue;
                }
                let (row, col) = (row0 + local_row as i64, col0 + local_col as i64);
                if let Some((height, texel)) = self.node(row, col) {
                    tile.heights.set_node(local_row, local_col, height);
                    tile.splat.set_texel(local_row, local_col, texel);
                }
            }
        }
        self.tiles.insert(coord, tile);
        Ok(())
    }

    /// Removes a tile and returns it.
    pub fn remove_tile(&mut self, coord: TileCoord) -> Option<TerrainTile> {
        self.tiles.remove(&coord)
    }

    /// The lowest and highest tile coordinates, or `None` with no tiles.
    pub fn coord_bounds(&self) -> Option<(TileCoord, TileCoord)> {
        let mut coords = self.tiles.keys();
        let first = *coords.next()?;
        Some(coords.fold((first, first), |(min, max), c| {
            (
                (min.0.min(c.0), min.1.min(c.1)),
                (max.0.max(c.0), max.1.max(c.1)),
            )
        }))
    }

    /// Terrain-local XZ corners of the rectangle the tiles span.
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let (min, max) = self.coord_bounds()?;
        let half = Vec2::splat(self.size * 0.5);
        Some((self.tile_center(min) - half, self.tile_center(max) + half))
    }

    /// Lowest and highest height over all tiles.
    pub fn height_range(&self) -> (f32, f32) {
        self.tiles
            .values()
            .flat_map(|tile| tile.heights.heights().iter().copied())
            .fold(None, |range: Option<(f32, f32)>, h| {
                Some(range.map_or((h, h), |(lo, hi)| (lo.min(h), hi.max(h))))
            })
            .unwrap_or((0.0, 0.0))
    }

    /// The tile(s) holding global node index `g` along one axis, with the
    /// node's local index in each.
    fn owners(&self, g: i64) -> [Option<(i32, usize)>; 2] {
        let n = self.cells_per_tile();
        let tile = g.div_euclid(n);
        let local = g.rem_euclid(n);
        let first = i32::try_from(tile).ok().map(|t| (t, local as usize));
        let second = if local == 0 {
            i32::try_from(tile - 1).ok().map(|t| (t, n as usize))
        } else {
            None
        };
        [first, second]
    }

    /// Every present copy of global node `(row, col)`: tile and local
    /// `(row, col)`.
    fn copies(&self, row: i64, col: i64) -> impl Iterator<Item = (TileCoord, usize, usize)> + '_ {
        let rows = self.owners(row);
        let cols = self.owners(col);
        rows.into_iter()
            .flatten()
            .flat_map(move |(tz, lr)| {
                cols.into_iter()
                    .flatten()
                    .map(move |(tx, lc)| ((tx, tz), lr, lc))
            })
            .filter(|(coord, _, _)| self.tiles.contains_key(coord))
    }

    fn node(&self, row: i64, col: i64) -> Option<(f32, [u8; MAX_TERRAIN_LAYERS])> {
        let (coord, local_row, local_col) = self.copies(row, col).next()?;
        let tile = self.tiles.get(&coord)?;
        Some((
            tile.heights.node_height(local_row as i64, local_col as i64),
            tile.splat.texel(local_row, local_col),
        ))
    }

    /// Height of global node `(row, col)`, or `None` if no tile holds it.
    pub fn node_height(&self, row: i64, col: i64) -> Option<f32> {
        self.node(row, col).map(|(height, _)| height)
    }

    /// Paint weights of global node `(row, col)`.
    pub fn node_weights(&self, row: i64, col: i64) -> Option<[u8; MAX_TERRAIN_LAYERS]> {
        self.node(row, col).map(|(_, texel)| texel)
    }

    fn set_height(&mut self, row: i64, col: i64, height: f32) {
        let copies: Vec<_> = self.copies(row, col).collect();
        for (coord, local_row, local_col) in copies {
            if let Some(tile) = self.tiles.get_mut(&coord) {
                tile.heights.set_node(local_row, local_col, height);
            }
        }
    }

    fn set_weights(&mut self, row: i64, col: i64, texel: [u8; MAX_TERRAIN_LAYERS]) {
        let copies: Vec<_> = self.copies(row, col).collect();
        for (coord, local_row, local_col) in copies {
            if let Some(tile) = self.tiles.get_mut(&coord) {
                tile.splat.set_texel(local_row, local_col, texel);
            }
        }
    }

    /// Terrain-local XZ of global node `(row, col)`.
    pub fn node_position(&self, row: i64, col: i64) -> Vec2 {
        let cell = self.cell_size();
        Vec2::new(col as f32 * cell, row as f32 * cell) - Vec2::splat(self.size * 0.5)
    }

    /// Unit normal at global node `(row, col)` from central differences
    /// across tile edges (one-sided where a neighbour is missing).
    pub fn node_normal(&self, row: i64, col: i64) -> Vec3 {
        let cell = self.cell_size();
        let Some(here) = self.node_height(row, col) else {
            return Vec3::Y;
        };
        let slope = |before: Option<f32>, after: Option<f32>| match (before, after) {
            (Some(b), Some(a)) => (a - b) / (2.0 * cell),
            (Some(b), None) => (here - b) / cell,
            (None, Some(a)) => (a - here) / cell,
            (None, None) => 0.0,
        };
        let dhdx = slope(
            self.node_height(row, col - 1),
            self.node_height(row, col + 1),
        );
        let dhdz = slope(
            self.node_height(row - 1, col),
            self.node_height(row + 1, col),
        );
        let normal = Vec3::new(-dhdx, 1.0, -dhdz).normalize_or_zero();
        if normal == Vec3::ZERO {
            Vec3::Y
        } else {
            normal
        }
    }

    /// The tile under terrain-local `(x, z)` and the point in that tile's
    /// own local space.
    fn tile_at(&self, x: f32, z: f32) -> Option<(&TerrainTile, Vec2)> {
        let half = self.size * 0.5;
        let tx = ((x + half) / self.size).floor() as i32;
        let tz = ((z + half) / self.size).floor() as i32;
        let slack = self.cell_size() * 1e-3;
        for coord in [(tx, tz), (tx - 1, tz), (tx, tz - 1), (tx - 1, tz - 1)] {
            if let Some(tile) = self.tiles.get(&coord) {
                let local = Vec2::new(x, z) - self.tile_center(coord);
                if local.abs().max_element() <= half + slack {
                    return Some((tile, local));
                }
            }
        }
        None
    }

    /// Height on the drawn triangles at terrain-local `(x, z)`, or `None`
    /// outside every tile. Matches the mesh and the collider.
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let (tile, local) = self.tile_at(x, z)?;
        Some(tile.heights.surface_height_at(local.x, local.y))
    }

    /// Interpolated unit normal at terrain-local `(x, z)`.
    pub fn normal_at(&self, x: f32, z: f32) -> Option<Vec3> {
        let (tile, local) = self.tile_at(x, z)?;
        Some(tile.heights.normal_at(local.x, local.y))
    }

    /// First hit of a terrain-local ray on the surface, as terrain-local XZ.
    pub fn raycast(&self, origin: Vec3, direction: Vec3) -> Option<Vec2> {
        if !origin.is_finite() || !direction.is_finite() || direction.length_squared() < 1e-12 {
            return None;
        }
        let direction = direction.normalize();
        let (min, max) = self.bounds()?;
        let mut near = 0.0f32;
        let mut far = 100_000.0f32;
        for (position, velocity, lo, hi) in [
            (origin.x, direction.x, min.x, max.x),
            (origin.z, direction.z, min.y, max.y),
        ] {
            if velocity.abs() < 1e-6 {
                if position < lo || position > hi {
                    return None;
                }
            } else {
                let a = (lo - position) / velocity;
                let b = (hi - position) / velocity;
                near = near.max(a.min(b));
                far = far.min(a.max(b));
            }
        }
        if far < near {
            return None;
        }
        let difference = |distance: f32| {
            let point = origin + direction * distance;
            self.height_at(point.x, point.z)
                .map(|height| point.y - height)
        };
        let step = self.cell_size() * 0.5;
        let steps = (((far - near) / step).ceil() as usize).clamp(64, 16_384);
        let mut previous_t = near;
        let mut previous = difference(near);
        if previous.is_some_and(|d| d.abs() < 1e-6) {
            let hit = origin + direction * near;
            return Some(Vec2::new(hit.x, hit.z));
        }
        for index in 1..=steps {
            let current_t = near + (far - near) * index as f32 / steps as f32;
            let current = difference(current_t);
            if let (Some(before), Some(now)) = (previous, current)
                && before > 0.0
                && now <= 0.0
            {
                let (mut low, mut high) = (previous_t, current_t);
                for _ in 0..20 {
                    let middle = (low + high) * 0.5;
                    if difference(middle).is_some_and(|d| d > 0.0) {
                        low = middle;
                    } else {
                        high = middle;
                    }
                }
                let hit = origin + direction * ((low + high) * 0.5);
                return Some(Vec2::new(hit.x, hit.z));
            }
            previous_t = current_t;
            previous = current;
        }
        None
    }

    /// Global nodes a brush could reach, clamped to the tiles' rectangle.
    fn brush_rect(&self, brush: &Brush) -> Option<DirtyRect> {
        if brush.radius <= 0.0 || !brush.radius.is_finite() || !brush.strength.is_finite() {
            return None;
        }
        let (min, max) = self.coord_bounds()?;
        let n = self.cells_per_tile();
        let half = self.size * 0.5;
        let cell = self.cell_size();
        let to_index = |v: f32| ((v + half) / cell).floor() as i64;
        let col_lo = i64::from(min.0) * n;
        let col_hi = i64::from(max.0) * n + n;
        let row_lo = i64::from(min.1) * n;
        let row_hi = i64::from(max.1) * n + n;
        let rect = DirtyRect {
            col_min: to_index(brush.center.x - brush.radius).clamp(col_lo, col_hi),
            col_max: (to_index(brush.center.x + brush.radius) + 1).clamp(col_lo, col_hi),
            row_min: to_index(brush.center.y - brush.radius).clamp(row_lo, row_hi),
            row_max: (to_index(brush.center.y + brush.radius) + 1).clamp(row_lo, row_hi),
        };
        (rect.col_min <= rect.col_max && rect.row_min <= rect.row_max).then_some(rect)
    }

    /// `(row, col, weight)` for every present node inside `brush`, where
    /// `weight` already includes the brush strength.
    fn affected(&self, brush: &Brush) -> Vec<(i64, i64, f32)> {
        let Some(rect) = self.brush_rect(brush) else {
            return Vec::new();
        };
        let mut nodes = Vec::new();
        for row in rect.row_min..=rect.row_max {
            for col in rect.col_min..=rect.col_max {
                let d = self.node_position(row, col).distance(brush.center);
                if d > brush.radius || self.copies(row, col).next().is_none() {
                    continue;
                }
                let weight = brush.falloff.weight(d / brush.radius) * brush.strength;
                if weight != 0.0 {
                    nodes.push((row, col, weight));
                }
            }
        }
        nodes
    }

    fn apply_heights(&mut self, updates: Vec<(i64, i64, f32)>) -> Option<DirtyRect> {
        let mut dirty = None;
        for (row, col, height) in updates {
            if self.node_height(row, col) == Some(height) || !height.is_finite() {
                continue;
            }
            self.set_height(row, col, height);
            dirty = union_dirty(dirty, Some(DirtyRect::point(row, col)));
        }
        dirty
    }

    /// Adds `strength * falloff` to every height inside `brush`. Returns the
    /// nodes that changed.
    pub fn raise_lower(&mut self, brush: &Brush) -> Option<DirtyRect> {
        let updates = self
            .affected(brush)
            .into_iter()
            .filter_map(|(row, col, w)| self.node_height(row, col).map(|h| (row, col, h + w)))
            .collect();
        self.apply_heights(updates)
    }

    /// Moves every height inside `brush` toward its 4-neighbour average,
    /// reading neighbours from before the edit (missing neighbours count as
    /// the node itself, as a lone [`Heightmap`] clamps at its edge).
    pub fn smooth(&mut self, brush: &Brush) -> Option<DirtyRect> {
        let updates = self
            .affected(brush)
            .into_iter()
            .filter_map(|(row, col, w)| {
                let here = self.node_height(row, col)?;
                let read = |r, c| self.node_height(r, c).unwrap_or(here);
                let average = (read(row - 1, col)
                    + read(row + 1, col)
                    + read(row, col - 1)
                    + read(row, col + 1))
                    * 0.25;
                Some((row, col, here + (average - here) * w.clamp(0.0, 1.0)))
            })
            .collect();
        self.apply_heights(updates)
    }

    /// Moves every height inside `brush` toward `target`.
    pub fn flatten(&mut self, brush: &Brush, target: f32) -> Option<DirtyRect> {
        if !target.is_finite() {
            return None;
        }
        let updates = self
            .affected(brush)
            .into_iter()
            .filter_map(|(row, col, w)| {
                let here = self.node_height(row, col)?;
                Some((row, col, here + (target - here) * w.clamp(0.0, 1.0)))
            })
            .collect();
        self.apply_heights(updates)
    }

    /// Paints `layer` into every texel inside `brush`; the other layers fade
    /// so each texel still adds up to 255. Returns the texels that changed.
    pub fn paint(&mut self, brush: &Brush, layer: usize) -> Option<DirtyRect> {
        if layer >= MAX_TERRAIN_LAYERS {
            return None;
        }
        let updates: Vec<_> = self
            .affected(brush)
            .into_iter()
            .filter_map(|(row, col, w)| {
                let before = self.node_weights(row, col)?;
                let after = blend_toward(before, layer, w);
                (after != before).then_some((row, col, after))
            })
            .collect();
        let mut dirty = None;
        for (row, col, texel) in updates {
            self.set_weights(row, col, texel);
            dirty = union_dirty(dirty, Some(DirtyRect::point(row, col)));
        }
        dirty
    }

    /// Every node inside the terrain-local XZ rectangle `min..max`, as
    /// `(row, col, position)`.
    fn nodes_in(&self, min: Vec2, max: Vec2) -> Vec<(i64, i64, Vec2)> {
        if !min.is_finite() || !max.is_finite() {
            return Vec::new();
        }
        let center = (min + max) * 0.5;
        let radius = ((max - min) * 0.5).max_element().max(self.cell_size());
        let brush = Brush {
            center,
            radius,
            strength: 1.0,
            falloff: crate::terrain::BrushFalloff::Constant,
        };
        let Some(rect) = self.brush_rect(&brush) else {
            return Vec::new();
        };
        let mut nodes = Vec::new();
        for row in rect.row_min..=rect.row_max {
            for col in rect.col_min..=rect.col_max {
                if self.copies(row, col).next().is_some() {
                    nodes.push((row, col, self.node_position(row, col)));
                }
            }
        }
        nodes
    }

    /// Shapes the ground inside the terrain-local rectangle `min..max` by
    /// `influence`, which gives each node's target height and how strongly
    /// (0–1) it moves there, or `None` to leave it. With `paint`, also paints
    /// that layer by the same weight times the given strength. Returns the
    /// nodes whose heights changed and the texels whose paint changed.
    pub fn shape_along(
        &mut self,
        min: Vec2,
        max: Vec2,
        influence: impl Fn(Vec2) -> Option<(f32, f32)>,
        paint: Option<(usize, f32)>,
    ) -> (Option<DirtyRect>, Option<DirtyRect>) {
        let nodes: Vec<(i64, i64, f32, f32)> = self
            .nodes_in(min, max)
            .into_iter()
            .filter_map(|(row, col, at)| {
                let (target, weight) = influence(at)?;
                (target.is_finite() && weight.is_finite() && weight > 0.0).then_some((
                    row,
                    col,
                    target,
                    weight.min(1.0),
                ))
            })
            .collect();
        let heights = nodes
            .iter()
            .filter_map(|&(row, col, target, weight)| {
                let here = self.node_height(row, col)?;
                Some((row, col, here + (target - here) * weight))
            })
            .collect();
        let height_dirty = self.apply_heights(heights);
        let mut paint_dirty = None;
        if let Some((layer, strength)) = paint
            && layer < MAX_TERRAIN_LAYERS
            && strength.is_finite()
        {
            for &(row, col, _, weight) in &nodes {
                let Some(before) = self.node_weights(row, col) else {
                    continue;
                };
                let after = blend_toward(before, layer, weight * strength.clamp(0.0, 1.0));
                if after != before {
                    self.set_weights(row, col, after);
                    paint_dirty = union_dirty(paint_dirty, Some(DirtyRect::point(row, col)));
                }
            }
        }
        (height_dirty, paint_dirty)
    }

    /// The local `(row, col)` range of a tile that a global rectangle
    /// covers, or `None` if it misses the tile.
    pub fn tile_local_rect(&self, coord: TileCoord, rect: DirtyRect) -> Option<DirtyRect> {
        let (row0, col0) = self.tile_origin(coord);
        let n = self.cells_per_tile();
        let local = DirtyRect {
            col_min: (rect.col_min - col0).max(0),
            col_max: (rect.col_max - col0).min(n),
            row_min: (rect.row_min - row0).max(0),
            row_max: (rect.row_max - row0).min(n),
        };
        (local.col_min <= local.col_max && local.row_min <= local.row_max).then_some(local)
    }

    /// Tiles a global rectangle touches.
    pub fn tiles_touching(&self, rect: DirtyRect) -> Vec<TileCoord> {
        self.tiles
            .keys()
            .copied()
            .filter(|coord| self.tile_local_rect(*coord, rect).is_some())
            .collect()
    }

    /// Every tile merged into one grid, for the heightfield collider.
    pub fn merged_grid(&self) -> Option<MergedGrid> {
        let (min, max) = self.coord_bounds()?;
        let n = self.cells_per_tile();
        let cols = (i64::from(max.0 - min.0) + 1) * n + 1;
        let rows = (i64::from(max.1 - min.1) + 1) * n + 1;
        let (row0, col0) = self.tile_origin(min);
        let mut heights = Vec::with_capacity((rows * cols) as usize);
        for row in 0..rows {
            for col in 0..cols {
                heights.push(self.node_height(row0 + row, col0 + col).unwrap_or(0.0));
            }
        }
        let mut cell_present = Vec::with_capacity(((rows - 1) * (cols - 1)) as usize);
        for row in 0..rows - 1 {
            for col in 0..cols - 1 {
                let coord = (min.0 + (col / n) as i32, min.1 + (row / n) as i32);
                cell_present.push(self.tiles.contains_key(&coord));
            }
        }
        Some(MergedGrid {
            rows: rows as usize,
            cols: cols as usize,
            heights,
            cell_present,
            min: self.node_position(row0, col0),
            cell: self.cell_size(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::BrushFalloff;

    fn surface(coords: &[TileCoord], resolution: u32, size: f32) -> TerrainSurface {
        let mut surface = TerrainSurface::new(resolution, size).unwrap();
        for coord in coords {
            surface
                .insert_tile(*coord, TerrainTile::flat(resolution, size).unwrap())
                .unwrap();
        }
        surface
    }

    /// Simple deterministic pseudo-random numbers for stroke tests.
    fn lcg(seed: &mut u64) -> f32 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*seed >> 33) as f32) / (1u64 << 31) as f32
    }

    #[test]
    fn shaping_along_a_band_moves_only_the_band_and_keeps_tile_edges_shared() {
        let (res, size) = (9u32, 8.0f32);
        let mut tiles = surface(&[(0, 0), (1, 0)], res, size);
        // A band 2 m either side of z = 0, across both tiles, raised to 3 m.
        let (heights, paint) = tiles.shape_along(
            Vec2::new(-4.0, -4.0),
            Vec2::new(12.0, 4.0),
            |at| (at.y.abs() <= 2.0).then_some((3.0, 1.0)),
            Some((1, 1.0)),
        );
        assert!(heights.is_some() && paint.is_some());
        assert_eq!(tiles.height_at(0.0, 0.0), Some(3.0));
        assert_eq!(tiles.height_at(4.0, 1.0), Some(3.0), "on the shared edge");
        assert_eq!(tiles.height_at(10.0, 0.0), Some(3.0));
        assert_eq!(tiles.height_at(0.0, 3.5), Some(0.0), "outside the band");
        let edge_left = tiles.tiles.get(&(0, 0)).unwrap().heights.node_height(4, 8);
        let edge_right = tiles.tiles.get(&(1, 0)).unwrap().heights.node_height(4, 0);
        assert_eq!(edge_left, edge_right);
        let (row, col) = (4, 4);
        assert_eq!(tiles.node_weights(row, col).map(|w| w[1]), Some(255));
    }

    #[test]
    fn two_tiles_edited_across_their_edge_match_one_combined_heightmap() {
        let (res, size) = (9u32, 8.0f32);
        let mut tiles = surface(&[(0, 0), (1, 0)], res, size);
        // The combined patch spans both tiles: [-4, 12] x [-4, 4], centred
        // at (4, 0), with 2 * (res - 1) + 1 = 17 columns.
        let mut combined = Heightmap::new(17, 16.0).unwrap();
        let combined_rows = 17;
        // A 17 x 17 combined map is taller than the 9-row tiles, so compare
        // only its middle rows; centre the strokes there.
        let shift = Vec2::new(4.0, 0.0);
        let strokes = [
            (Brush::new(Vec2::new(4.0, 0.0), 3.0, 1.5), 0),
            (Brush::new(Vec2::new(3.0, 1.0), 2.5, 0.8), 1),
            (Brush::new(Vec2::new(5.0, -1.0), 2.0, 0.6), 2),
        ];
        for (brush, op) in strokes {
            let shifted = Brush {
                center: brush.center - shift,
                ..brush
            };
            match op {
                0 => {
                    tiles.raise_lower(&brush);
                    combined.raise_lower(&shifted);
                }
                1 => {
                    tiles.smooth(&brush);
                    combined.smooth(&shifted);
                }
                _ => {
                    tiles.flatten(&brush, 0.4);
                    combined.flatten(&shifted, 0.4);
                }
            }
        }
        // Combined rows 4..=12 line up with tile rows 0..=8.
        for row in 0..res as i64 {
            for col in 0..combined_rows as i64 {
                let tile_height = tiles.node_height(row, col).unwrap();
                let one = combined.node_height(row + 4, col);
                assert!(
                    (tile_height - one).abs() < 1e-5,
                    "({row}, {col}): {tile_height} vs {one}"
                );
            }
        }
    }

    #[test]
    fn shared_edge_nodes_stay_identical_and_normals_agree_across_the_edge() {
        let res = 17;
        let mut tiles = surface(&[(0, 0), (1, 0), (0, 1), (1, 1)], res, 16.0);
        let mut seed = 7;
        for index in 0..40 {
            let center = Vec2::new(lcg(&mut seed) * 24.0 - 8.0, lcg(&mut seed) * 24.0 - 8.0);
            let brush = Brush::new(
                center,
                1.0 + lcg(&mut seed) * 4.0,
                lcg(&mut seed) * 2.0 - 1.0,
            );
            match index % 3 {
                0 => tiles.raise_lower(&brush),
                1 => tiles.smooth(&brush),
                _ => tiles.paint(&brush, index % 4),
            };
        }
        let n = (res - 1) as usize;
        let east = tiles.tile((1, 0)).unwrap();
        let west = tiles.tile((0, 0)).unwrap();
        let south = tiles.tile((0, 1)).unwrap();
        for i in 0..=n {
            let w = west.heights.node_height(i as i64, n as i64);
            let e = east.heights.node_height(i as i64, 0);
            assert_eq!(w.to_bits(), e.to_bits(), "row {i}");
            assert_eq!(west.splat.texel(i, n), east.splat.texel(i, 0));
            let s = south.heights.node_height(0, i as i64);
            let north = west.heights.node_height(n as i64, i as i64);
            assert_eq!(s.to_bits(), north.to_bits(), "col {i}");
        }
        // The normal at an edge node is one value, whichever tile asks.
        let a = tiles.node_normal(5, n as i64);
        assert!((a.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_new_tile_takes_the_shared_edge_from_its_neighbour() {
        let mut tiles = surface(&[(0, 0)], 9, 8.0);
        tiles.raise_lower(&Brush::new(Vec2::new(4.0, 0.0), 2.0, 1.0));
        let edge: Vec<f32> = (0..9)
            .map(|row| tiles.node_height(row, 8).unwrap())
            .collect();
        tiles
            .insert_tile((1, 0), TerrainTile::flat(9, 8.0).unwrap())
            .unwrap();
        let east = tiles.tile((1, 0)).unwrap();
        for (row, height) in edge.iter().enumerate() {
            assert_eq!(east.heights.node_height(row as i64, 0), *height);
        }
    }

    #[test]
    fn paint_keeps_every_texel_summing_to_255_and_fades_other_layers() {
        let mut tiles = surface(&[(0, 0)], 9, 8.0);
        let brush = Brush {
            center: Vec2::ZERO,
            radius: 3.0,
            strength: 0.37,
            falloff: BrushFalloff::Smooth,
        };
        for layer in [1, 2, 7, 3, 1] {
            assert!(tiles.paint(&brush, layer).is_some());
        }
        let tile = tiles.tile((0, 0)).unwrap();
        for texel in tile.splat.weights() {
            let sum: u32 = texel.iter().map(|w| u32::from(*w)).sum();
            assert_eq!(sum, 255);
        }
        // The centre is now mostly layer 1; outside the brush is untouched.
        let centre = tiles.node_weights(4, 4).unwrap();
        assert!(centre[1] > centre[0]);
        assert_eq!(tiles.node_weights(0, 0).unwrap()[0], 255);
        assert!(tile.splat.uses_second_map());
        assert!(tiles.paint(&brush, MAX_TERRAIN_LAYERS).is_none());
    }

    #[test]
    fn splat_rgba_round_trips_and_normalizes() {
        let mut tiles = surface(&[(0, 0)], 5, 4.0);
        tiles.paint(&Brush::new(Vec2::ZERO, 2.0, 0.6), 5);
        let splat = &tiles.tile((0, 0)).unwrap().splat;
        let back = SplatMap::from_rgba(5, &splat.rgba(0), Some(&splat.rgba(1))).unwrap();
        assert_eq!(&back, splat);
        let zeros = SplatMap::from_rgba(2, &[0; 16], None).unwrap();
        assert!(zeros.weights().iter().all(|t| t[0] == 255));
        let uneven = SplatMap::from_rgba(1, &[10, 10, 0, 0], None).unwrap();
        let sum: u32 = uneven.weights()[0].iter().map(|w| u32::from(*w)).sum();
        assert_eq!(sum, 255);
        assert!(SplatMap::from_rgba(2, &[0; 15], None).is_err());
    }

    #[test]
    fn caps_reject_too_many_tiles_mismatched_tiles_and_too_many_samples() {
        let mut tiles = surface(&[], 2, 1.0);
        for index in 0..MAX_TERRAIN_TILES as i32 {
            tiles
                .insert_tile((index % 8, index / 8), TerrainTile::flat(2, 1.0).unwrap())
                .unwrap();
        }
        assert!(
            tiles
                .insert_tile((9, 9), TerrainTile::flat(2, 1.0).unwrap())
                .is_err()
        );
        // Replacing an existing tile is fine at the cap.
        assert!(
            tiles
                .insert_tile((0, 0), TerrainTile::flat(2, 1.0).unwrap())
                .is_ok()
        );

        let mut mixed = surface(&[(0, 0)], 9, 8.0);
        assert!(
            mixed
                .insert_tile((1, 0), TerrainTile::flat(5, 8.0).unwrap())
                .is_err()
        );
        assert!(
            mixed
                .insert_tile((1, 0), TerrainTile::flat(9, 4.0).unwrap())
                .is_err()
        );

        let mut big = surface(&[(0, 0)], 1024, 100.0);
        assert!(
            big.insert_tile((4, 4), TerrainTile::flat(1024, 100.0).unwrap())
                .is_err()
        );
    }

    #[test]
    fn height_at_follows_the_drawn_triangles() {
        // One cell with a raised bottom-right corner: on the top-left to
        // bottom-right diagonal the height runs straight from 0 to 1, and
        // the off-diagonal corners stay flat.
        let mut heights = Heightmap::new(2, 2.0).unwrap();
        heights.set_node(1, 1, 1.0);
        let mut tiles = TerrainSurface::new(2, 2.0).unwrap();
        tiles
            .insert_tile(
                (0, 0),
                TerrainTile {
                    heights,
                    splat: SplatMap::new(2),
                },
            )
            .unwrap();
        assert!((tiles.height_at(0.0, 0.0).unwrap() - 0.5).abs() < 1e-6);
        // Upper-right triangle (tl, tr, br): plane h = tz.
        assert!((tiles.height_at(0.8, -0.6).unwrap() - 0.2).abs() < 1e-6);
        // Lower-left triangle (tl, bl, br): plane h = tx.
        assert!((tiles.height_at(-0.6, 0.8).unwrap() - 0.2).abs() < 1e-6);
        assert!(tiles.height_at(5.0, 0.0).is_none());
    }

    #[test]
    fn raycast_finds_hits_across_tiles_and_misses_gaps() {
        let tiles = surface(&[(0, 0), (2, 0)], 9, 8.0);
        let hit = tiles
            .raycast(Vec3::new(1.0, 5.0, 2.0), Vec3::NEG_Y)
            .unwrap();
        assert!(hit.distance(Vec2::new(1.0, 2.0)) < 1e-3);
        let far = tiles
            .raycast(Vec3::new(17.0, 5.0, 0.0), Vec3::NEG_Y)
            .unwrap();
        assert!(far.distance(Vec2::new(17.0, 0.0)) < 1e-3);
        // Tile (1, 0) is missing.
        assert!(
            tiles
                .raycast(Vec3::new(8.0, 5.0, 0.0), Vec3::NEG_Y)
                .is_none()
        );
        let slanted = tiles
            .raycast(Vec3::new(-2.0, 4.0, 0.0), Vec3::new(1.0, -1.0, 0.0))
            .unwrap();
        assert!(slanted.distance(Vec2::new(2.0, 0.0)) < 1e-2);
    }

    #[test]
    fn merged_grid_covers_the_tile_rectangle_and_marks_missing_cells() {
        let mut tiles = surface(&[(0, 0), (1, 1)], 3, 2.0);
        tiles.raise_lower(&Brush::new(Vec2::new(2.0, 2.0), 0.5, 1.0));
        let grid = tiles.merged_grid().unwrap();
        assert_eq!((grid.rows, grid.cols), (5, 5));
        assert_eq!(grid.cell_present.len(), 16);
        // Cells of tile (0, 0) and (1, 1) present, (1, 0) and (0, 1) not.
        assert!(grid.cell_present[0]);
        assert!(!grid.cell_present[3]);
        assert!(!grid.cell_present[12]);
        assert!(grid.cell_present[15]);
        assert_eq!(grid.min, Vec2::new(-1.0, -1.0));
        assert_eq!(grid.center(), Vec2::new(1.0, 1.0));
        assert!((grid.heights[3 * 5 + 3] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn dirty_rect_reports_only_the_changed_nodes() {
        let mut tiles = surface(&[(0, 0)], 33, 32.0);
        let rect = tiles
            .raise_lower(&Brush::new(Vec2::new(-10.0, -10.0), 2.0, 1.0))
            .unwrap();
        assert!(rect.col_max <= 9 && rect.row_max <= 9);
        assert!(rect.col_min >= 3 && rect.row_min >= 3);
        // A flatten to the current height changes nothing.
        let mut flat = surface(&[(0, 0)], 9, 8.0);
        assert!(
            flat.flatten(&Brush::new(Vec2::ZERO, 2.0, 1.0), 0.0)
                .is_none()
        );
        assert_eq!(tiles.tiles_touching(rect), vec![(0, 0)]);
    }
}
