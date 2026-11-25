//! Grid navigation: a walkable cell grid over the world's XZ plane, A*
//! pathfinding on it, grid line-of-sight, path smoothing, and path
//! following.
//!
//! The grid spans `width x height` square cells of `cell_size` world
//! units, with cell `(0, 0)`'s minimum corner at `origin` (a world-space
//! `(x, z)`). [`GridCoord`]'s `x` maps to world X, `y` to world Z. Every
//! cell is walkable until blocked ([`NavGrid::set_blocked`],
//! [`NavGrid::block_aabb`], [`NavGrid::block_circle`]); coordinates
//! outside the grid count as blocked.
//!
//! A cell can hold more than one floor ("layers"): the ground, a bridge
//! over it, an upper storey. Layer `0` is the lowest; [`NavNode`] names
//! one floor of one cell. Neighbouring floors connect when the height
//! change between them is within the grid's climb. The plain
//! [`GridCoord`] functions work on layer `0`.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::{Vec2, Vec3};

use crate::shapes::{Clearance, Solid};

/// Most floors one cell holds.
pub const MAX_LAYERS: usize = 4;

/// Cost of an orthogonal (N/S/E/W) step between adjacent cells.
const ORTHOGONAL_COST: f32 = 1.0;
/// Cost of a diagonal step — `sqrt(2)`, so diagonal moves are priced
/// correctly against two orthogonal ones.
const DIAGONAL_COST: f32 = std::f32::consts::SQRT_2;

/// Upper bound on `width * height`, so an untrusted grid size can't ask
/// for a multi-gigabyte allocation. 4 million cells (e.g. 2000x2000) is
/// far past what grid A* stays interactive on anyway.
const MAX_CELLS: u64 = 4_000_000;

/// Neighbour offsets: the four orthogonal directions first, then the four
/// diagonals (so index `>= 4` identifies a diagonal step).
const NEIGHBOR_OFFSETS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

/// Errors from constructing a [`NavGrid`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NavError {
    /// A grid dimension was zero.
    #[error("navigation grid dimensions must be nonzero (got {width}x{height})")]
    InvalidDimensions {
        /// Requested width, in cells.
        width: u32,
        /// Requested height, in cells.
        height: u32,
    },
    /// `cell_size` was not positive and finite.
    #[error("navigation grid cell size must be positive and finite (got {cell_size})")]
    InvalidCellSize {
        /// The rejected cell size.
        cell_size: f32,
    },
    /// The grid origin was not finite.
    #[error("navigation grid origin must be finite (got {origin:?})")]
    InvalidOrigin {
        /// The rejected world-space origin.
        origin: Vec2,
    },
    /// A height list did not have one finite value per cell.
    #[error("navigation grid needs one finite height per cell (got {count} for {cells} cells)")]
    InvalidHeights {
        /// Heights given.
        count: usize,
        /// Cells in the grid.
        cells: usize,
    },
    /// The floors above the first were not one sorted list of up to
    /// [`MAX_LAYERS`]` - 1` per cell with finite heights.
    #[error("navigation grid layers are malformed: {reason}")]
    InvalidLayers {
        /// What was wrong.
        reason: &'static str,
    },
    /// `width * height` exceeded the internal cell-count limit.
    #[error("navigation grid too large: {cells} cells exceeds the {max} limit")]
    GridTooLarge {
        /// Requested cell count.
        cells: u64,
        /// The maximum allowed.
        max: u64,
    },
}

/// An integer cell coordinate in a [`NavGrid`]. `x` is along world X, `y`
/// along world Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridCoord {
    /// Column index (world X).
    pub x: i32,
    /// Row index (world Z).
    pub y: i32,
}

impl GridCoord {
    /// A coordinate `(x, y)`.
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// One floor of one cell: layer `0` is the lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NavNode {
    /// The cell.
    pub coord: GridCoord,
    /// Which of its floors, from the bottom.
    pub layer: u8,
}

impl NavNode {
    /// The lowest floor of `coord`.
    pub fn ground(coord: GridCoord) -> Self {
        Self { coord, layer: 0 }
    }
}

/// A floor above a cell's lowest one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    /// The cell, row-major.
    pub cell: u32,
    /// Its height.
    pub height: f32,
    /// Whether characters cannot stand there.
    pub blocked: bool,
}

/// A uniform grid of walkable / blocked cells over the world's XZ plane.
#[derive(Debug, Clone, PartialEq)]
pub struct NavGrid {
    width: u32,
    height: u32,
    cell_size: f32,
    origin: Vec2,
    /// Row-major, `width * height`. `true` = blocked.
    blocked: Vec<bool>,
    /// Row-major ground height per cell, or empty for a flat grid at `0`.
    heights: Vec<f32>,
    /// Floors above each cell's lowest, all cells' in order: cell `i` owns
    /// `upper[upper_start[i]..upper_start[i + 1]]`. Both empty on a grid
    /// with one floor.
    upper: Vec<Span>,
    upper_start: Vec<u32>,
    /// The most a step to a neighbouring floor may rise or drop, or `None`
    /// to ignore heights.
    climb: Option<f32>,
    /// Floors something moving stands on for now, by node index (cells
    /// first, then `upper`). Empty when nothing does.
    moving: Vec<bool>,
}

impl NavGrid {
    /// A grid of `width` x `height` cells, each `cell_size` world units
    /// square, with cell `(0, 0)`'s minimum corner at `origin` (a
    /// world-space `(x, z)`). Every cell starts walkable.
    ///
    /// # Errors
    ///
    /// - [`NavError::InvalidDimensions`] if `width` or `height` is `0`.
    /// - [`NavError::InvalidCellSize`] if `cell_size <= 0` or non-finite.
    /// - [`NavError::GridTooLarge`] if `width * height` exceeds the
    ///   internal cell-count limit.
    pub fn new(width: u32, height: u32, cell_size: f32, origin: Vec2) -> Result<Self, NavError> {
        if width == 0 || height == 0 {
            return Err(NavError::InvalidDimensions { width, height });
        }
        if cell_size <= 0.0 || !cell_size.is_finite() {
            return Err(NavError::InvalidCellSize { cell_size });
        }
        if !origin.is_finite() {
            return Err(NavError::InvalidOrigin { origin });
        }
        let cells = u64::from(width) * u64::from(height);
        if cells > MAX_CELLS {
            return Err(NavError::GridTooLarge {
                cells,
                max: MAX_CELLS,
            });
        }
        Ok(Self {
            width,
            height,
            cell_size,
            origin,
            blocked: vec![false; cells as usize],
            heights: Vec::new(),
            upper: Vec::new(),
            upper_start: Vec::new(),
            climb: None,
            moving: Vec::new(),
        })
    }

    /// This grid with floors above the lowest one in some cells, and the
    /// most a step between neighbouring floors may rise or drop (`None`
    /// connects every neighbour, as on a grid with one floor).
    ///
    /// `upper` is sorted by cell, then by height, and
    /// a cell has at most [`MAX_LAYERS`]` - 1` of them.
    ///
    /// # Errors
    ///
    /// [`NavError::InvalidLayers`] when `upper` breaks those rules or
    /// `climb` is negative or not finite.
    pub fn with_layers(mut self, upper: Vec<Span>, climb: Option<f32>) -> Result<Self, NavError> {
        let bad = |reason| Err(NavError::InvalidLayers { reason });
        if climb.is_some_and(|climb| !climb.is_finite() || climb < 0.0) {
            return bad("the climb is not a finite, non-negative height");
        }
        let cells = self.blocked.len();
        let mut start = vec![0u32; cells + 1];
        for (index, span) in upper.iter().enumerate() {
            let cell = span.cell as usize;
            if cell >= cells {
                return bad("a floor is outside the grid");
            }
            if !span.height.is_finite() {
                return bad("a floor height is not finite");
            }
            if let Some(previous) = index.checked_sub(1).and_then(|i| upper.get(i))
                && (previous.cell > span.cell
                    || (previous.cell == span.cell && previous.height >= span.height))
            {
                return bad("floors are not sorted by cell and height");
            }
            start[cell + 1] += 1;
            if start[cell + 1] as usize >= MAX_LAYERS {
                return bad("a cell has too many floors");
            }
        }
        for cell in 0..cells {
            start[cell + 1] += start[cell];
        }
        self.upper_start = if upper.is_empty() { Vec::new() } else { start };
        self.upper = upper;
        self.climb = climb;
        self.moving.clear();
        Ok(self)
    }

    /// This grid with a ground height per cell (row-major), so agents
    /// walking it can follow hills. An empty list keeps the grid flat.
    ///
    /// # Errors
    ///
    /// [`NavError::InvalidHeights`] if `heights` is neither empty nor one
    /// finite value per cell.
    pub fn with_heights(mut self, heights: Vec<f32>) -> Result<Self, NavError> {
        if !heights.is_empty()
            && (heights.len() != self.blocked.len() || !heights.iter().all(|h| h.is_finite()))
        {
            return Err(NavError::InvalidHeights {
                count: heights.len(),
                cells: self.blocked.len(),
            });
        }
        self.heights = heights;
        Ok(self)
    }

    /// Builds a grid straight from row-major blocked flags — what a
    /// scene file carries.
    ///
    /// The caller owns the meaning of the flags; this only checks there
    /// is exactly one per cell, so a truncated file is an error rather
    /// than an out-of-bounds read the first time something pathfinds.
    ///
    /// # Errors
    ///
    /// Whatever [`NavGrid::new`] rejects, plus
    /// [`NavError::InvalidDimensions`] if `blocked.len()` is not
    /// `width * height`.
    pub fn from_cells(
        width: u32,
        height: u32,
        cell_size: f32,
        origin: Vec2,
        blocked: &[bool],
    ) -> Result<Self, NavError> {
        let mut grid = Self::new(width, height, cell_size, origin)?;
        if blocked.len() != grid.blocked.len() {
            return Err(NavError::InvalidDimensions { width, height });
        }
        grid.blocked.copy_from_slice(blocked);
        Ok(grid)
    }

    /// Width, in cells.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height, in cells.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Side length of one cell, in world units.
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// World-space `(x, z)` of cell `(0, 0)`'s minimum corner.
    pub fn origin(&self) -> Vec2 {
        self.origin
    }

    /// Whether `coord` lies within the grid.
    pub fn in_bounds(&self, coord: GridCoord) -> bool {
        coord.x >= 0
            && coord.y >= 0
            && (coord.x as u32) < self.width
            && (coord.y as u32) < self.height
    }

    fn index(&self, coord: GridCoord) -> Option<usize> {
        if self.in_bounds(coord) {
            Some(coord.y as usize * self.width as usize + coord.x as usize)
        } else {
            None
        }
    }

    /// Whether `coord` is blocked. **Out-of-bounds coordinates count as
    /// blocked** — pathfinding never needs to reason about "off the grid"
    /// as a distinct state.
    /// Something moving standing on the lowest floor blocks it too.
    pub fn is_blocked(&self, coord: GridCoord) -> bool {
        !self.is_walkable(coord)
    }

    /// Whether `coord` is in bounds and its lowest floor is not blocked.
    pub fn is_walkable(&self, coord: GridCoord) -> bool {
        self.is_node_walkable(NavNode::ground(coord))
    }

    /// Sets `coord`'s blocked state. Out-of-bounds coordinates are
    /// ignored.
    pub fn set_blocked(&mut self, coord: GridCoord, blocked: bool) {
        if let Some(index) = self.index(coord) {
            self.blocked[index] = blocked;
        }
    }

    /// The ground height of the cell holding `world_xz`, clamped to the
    /// nearest edge cell. `0` on a flat grid.
    pub fn height_at(&self, world_xz: Vec2) -> f32 {
        if self.heights.is_empty() {
            return 0.0;
        }
        let coord = self.world_to_coord(world_xz);
        let x = coord.x.clamp(0, self.width as i32 - 1) as usize;
        let y = coord.y.clamp(0, self.height as i32 - 1) as usize;
        self.heights[y * self.width as usize + x]
    }

    /// The walkable cell closest to `coord` within `max_rings` rings of
    /// cells around it, `coord` itself first. Agents and targets often stand
    /// in a blocked cell (against a wall the bake grew by the agent's
    /// radius), and a route still has to start and end somewhere.
    pub fn nearest_walkable(&self, coord: GridCoord, max_rings: i32) -> Option<GridCoord> {
        if self.is_walkable(coord) {
            return Some(coord);
        }
        for ring in 1..=max_rings.max(0) {
            let mut best: Option<(i32, GridCoord)> = None;
            for dy in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs() != ring && dy.abs() != ring {
                        continue;
                    }
                    let candidate = GridCoord::new(coord.x + dx, coord.y + dy);
                    let distance = dx * dx + dy * dy;
                    if self.is_walkable(candidate) && best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, candidate));
                    }
                }
            }
            if let Some((_, found)) = best {
                return Some(found);
            }
        }
        None
    }

    /// The cell containing world position `world_xz`. May be out of bounds
    /// (check with [`NavGrid::in_bounds`]).
    pub fn world_to_coord(&self, world_xz: Vec2) -> GridCoord {
        let local = (world_xz - self.origin) / self.cell_size;
        GridCoord {
            x: local.x.floor() as i32,
            y: local.y.floor() as i32,
        }
    }

    /// The world-space centre of cell `coord`.
    pub fn coord_to_world_center(&self, coord: GridCoord) -> Vec2 {
        self.origin
            + Vec2::new(
                (coord.x as f32 + 0.5) * self.cell_size,
                (coord.y as f32 + 0.5) * self.cell_size,
            )
    }

    /// The inclusive grid-index rectangle `[x0, x1] x [y0, y1]`, clamped
    /// to the grid, that a world-space AABB from `lo` to `hi` overlaps.
    fn clamped_cell_range(&self, lo: Vec2, hi: Vec2) -> (i32, i32, i32, i32) {
        let c0 = self.world_to_coord(lo);
        let c1 = self.world_to_coord(hi);
        (
            c0.x.clamp(0, self.width as i32 - 1),
            c1.x.clamp(0, self.width as i32 - 1),
            c0.y.clamp(0, self.height as i32 - 1),
            c1.y.clamp(0, self.height as i32 - 1),
        )
    }

    /// Blocks every cell overlapping the world-space axis-aligned box with
    /// corners `min` and `max` (order-independent). A box wholly outside
    /// the grid blocks nothing.
    pub fn block_aabb(&mut self, min: Vec2, max: Vec2) {
        let lo = min.min(max);
        let hi = min.max(max);
        // Reject a box that doesn't reach the grid at all, so the clamp
        // below can't collapse a miss into blocking edge cells.
        let grid_min = self.origin;
        let grid_max = self.origin
            + Vec2::new(
                self.width as f32 * self.cell_size,
                self.height as f32 * self.cell_size,
            );
        if hi.x < grid_min.x || lo.x > grid_max.x || hi.y < grid_min.y || lo.y > grid_max.y {
            return;
        }
        let (x0, x1, y0, y1) = self.clamped_cell_range(lo, hi);
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set_blocked(GridCoord { x, y }, true);
            }
        }
    }

    /// Blocks every cell whose centre lies within `radius` of world-space
    /// `center`. A non-positive or non-finite `radius` does nothing.
    pub fn block_circle(&mut self, center: Vec2, radius: f32) {
        if radius <= 0.0 || !radius.is_finite() {
            return;
        }
        let (x0, x1, y0, y1) =
            self.clamped_cell_range(center - Vec2::splat(radius), center + Vec2::splat(radius));
        let radius_sq = radius * radius;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let coord = GridCoord { x, y };
                if self.coord_to_world_center(coord).distance_squared(center) <= radius_sq {
                    self.set_blocked(coord, true);
                }
            }
        }
    }
}

impl NavGrid {
    /// Whether the grid has heights: hills, or more than one floor.
    pub fn has_heights(&self) -> bool {
        !self.heights.is_empty() || !self.upper.is_empty()
    }

    /// Whether some cell has more than one floor.
    pub fn has_layers(&self) -> bool {
        !self.upper.is_empty()
    }

    /// The most a step between neighbouring floors may rise or drop, or
    /// `None` when heights are ignored.
    pub fn climb(&self) -> Option<f32> {
        self.climb
    }

    /// How many floors `coord` has: `0` outside the grid.
    pub fn layer_count(&self, coord: GridCoord) -> u8 {
        let Some(cell) = self.index(coord) else {
            return 0;
        };
        let above = match (self.upper_start.get(cell), self.upper_start.get(cell + 1)) {
            (Some(start), Some(end)) => end - start,
            _ => 0,
        };
        1 + above as u8
    }

    /// The node's index: cells first, then the floors above them.
    fn node_index(&self, node: NavNode) -> Option<usize> {
        let cell = self.index(node.coord)?;
        if node.layer == 0 {
            return Some(cell);
        }
        let start = *self.upper_start.get(cell)? as usize;
        let end = *self.upper_start.get(cell + 1)? as usize;
        let at = start + node.layer as usize - 1;
        (at < end).then_some(self.blocked.len() + at)
    }

    /// The floor's height and whether it is blocked (by the bake, or by
    /// something moving).
    fn span(&self, node: NavNode) -> Option<(f32, bool)> {
        let index = self.node_index(node)?;
        let moving = self.moving.get(index).copied().unwrap_or(false);
        if index < self.blocked.len() {
            let height = self.heights.get(index).copied().unwrap_or(0.0);
            Some((height, self.blocked[index] || moving))
        } else {
            let span = self.upper.get(index - self.blocked.len())?;
            Some((span.height, span.blocked || moving))
        }
    }

    /// The height of `node`'s floor, if it exists.
    pub fn node_height(&self, node: NavNode) -> Option<f32> {
        self.span(node).map(|(height, _)| height)
    }

    /// Whether `node` exists and characters can stand there.
    pub fn is_node_walkable(&self, node: NavNode) -> bool {
        self.span(node).is_some_and(|(_, blocked)| !blocked)
    }

    /// Whether a step between floors at heights `a` and `b` is allowed.
    fn connects(&self, a: f32, b: f32) -> bool {
        self.climb.is_none_or(|climb| (a - b).abs() <= climb + 1e-4)
    }

    /// The walkable floor of `to` reached by a step from a floor at
    /// `from_height`: the one closest in height, if any is within the
    /// climb.
    fn step_to(&self, from_height: f32, to: GridCoord) -> Option<(NavNode, f32)> {
        (0..self.layer_count(to))
            .map(|layer| NavNode { coord: to, layer })
            .filter_map(|node| match self.span(node) {
                Some((height, false)) if self.connects(from_height, height) => Some((node, height)),
                _ => None,
            })
            .min_by(|a, b| {
                (a.1 - from_height)
                    .abs()
                    .total_cmp(&(b.1 - from_height).abs())
            })
    }

    /// How far below a floor something may be and still count as standing
    /// on it.
    fn stand_reach(&self) -> f32 {
        self.climb.unwrap_or(0.0).max(0.5)
    }

    /// The floor of the cell holding `xz` that something at `height`
    /// stands on: the highest one not far above it, or the lowest.
    pub fn node_at(&self, xz: Vec2, height: f32) -> NavNode {
        let coord = self.world_to_coord(xz);
        let reach = self.stand_reach();
        let layer = (0..self.layer_count(coord))
            .rev()
            .find(|layer| {
                self.node_height(NavNode {
                    coord,
                    layer: *layer,
                })
                .is_some_and(|floor| floor <= height + reach)
            })
            .unwrap_or(0);
        NavNode { coord, layer }
    }

    /// The height of the floor something at `xz` and `height` stands on,
    /// clamped to the nearest edge cell like [`NavGrid::height_at`].
    pub fn floor_height(&self, xz: Vec2, height: f32) -> f32 {
        if !self.has_layers() {
            return self.height_at(xz);
        }
        let coord = self.world_to_coord(xz);
        let clamped = GridCoord::new(
            coord.x.clamp(0, self.width as i32 - 1),
            coord.y.clamp(0, self.height as i32 - 1),
        );
        let node = self.node_at(self.coord_to_world_center(clamped), height);
        self.node_height(node).unwrap_or(0.0)
    }

    /// The walkable floor closest to `xz` at `height`, within `max_rings`
    /// rings of cells: the floor stood on first, then the nearest one on
    /// the same level.
    pub fn nearest_walkable_node(&self, xz: Vec2, height: f32, max_rings: i32) -> Option<NavNode> {
        let start = self.node_at(xz, height);
        if self.is_node_walkable(start) {
            return Some(start);
        }
        let level = self.node_height(start).unwrap_or(height);
        let climb = self.stand_reach();
        let coord = start.coord;
        for ring in 1..=max_rings.max(0) {
            let reach = climb * (ring as f32 + 1.0);
            let mut best: Option<(f32, NavNode)> = None;
            for dy in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs() != ring && dy.abs() != ring {
                        continue;
                    }
                    let cell = GridCoord::new(coord.x + dx, coord.y + dy);
                    for layer in 0..self.layer_count(cell) {
                        let node = NavNode { coord: cell, layer };
                        let Some((floor, false)) = self.span(node) else {
                            continue;
                        };
                        let rise = (floor - level).abs();
                        if self.climb.is_some() && rise > reach {
                            continue;
                        }
                        let score = (dx * dx + dy * dy) as f32 + rise * 1e-3;
                        if best.is_none_or(|(s, _)| score < s) {
                            best = Some((score, node));
                        }
                    }
                }
            }
            if let Some((_, found)) = best {
                return Some(found);
            }
        }
        None
    }

    /// Every floor of every cell: its node, height and whether it is
    /// blocked.
    pub fn floors(&self) -> impl Iterator<Item = (NavNode, f32, bool)> + '_ {
        (0..self.height as i32).flat_map(move |y| {
            (0..self.width as i32).flat_map(move |x| {
                let coord = GridCoord::new(x, y);
                (0..self.layer_count(coord)).filter_map(move |layer| {
                    let node = NavNode { coord, layer };
                    self.span(node)
                        .map(|(height, blocked)| (node, height, blocked))
                })
            })
        })
    }

    /// Marks the floors the moving `solids` stand in as blocked, clearing
    /// the ones they left. `clearance` is the space a character needs.
    /// Returns whether any floor changed.
    pub fn block_moving(&mut self, solids: &[Solid], clearance: &Clearance) -> bool {
        let nodes = self.blocked.len() + self.upper.len();
        let mut next = vec![false; nodes];
        let half = Vec2::splat(self.cell_size * 0.5);
        for solid in solids {
            let (lo, hi) = solid.bounds();
            let grow = Vec2::splat(clearance.radius.max(0.0));
            let (x0, x1, y0, y1) =
                self.clamped_cell_range(Vec2::new(lo.x, lo.z) - grow, Vec2::new(hi.x, hi.z) + grow);
            if !self.box_reaches_grid(Vec2::new(lo.x, lo.z) - grow, Vec2::new(hi.x, hi.z) + grow) {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let coord = GridCoord::new(x, y);
                    let centre = self.coord_to_world_center(coord);
                    for layer in 0..self.layer_count(coord) {
                        let node = NavNode { coord, layer };
                        let (Some(index), Some(floor)) =
                            (self.node_index(node), self.node_height(node))
                        else {
                            continue;
                        };
                        if !next[index]
                            && solid.blocks_floor(centre - half, centre + half, floor, clearance)
                        {
                            next[index] = true;
                        }
                    }
                }
            }
        }
        if !next.iter().any(|blocked| *blocked) {
            next.clear();
        }
        let changed = next != self.moving;
        self.moving = next;
        changed
    }

    /// Whether the `(x, z)` box from `lo` to `hi` reaches the grid.
    fn box_reaches_grid(&self, lo: Vec2, hi: Vec2) -> bool {
        let grid_max = self.origin
            + Vec2::new(
                self.width as f32 * self.cell_size,
                self.height as f32 * self.cell_size,
            );
        !(hi.x < self.origin.x || lo.x > grid_max.x || hi.y < self.origin.y || lo.y > grid_max.y)
    }
}

/// Octile heuristic: the exact cost of the cheapest obstacle-free 8-way
/// path between two cells (diagonal moves as far as possible, then
/// straight). Admissible and consistent for [`find_path`]'s cost model.
fn octile_heuristic(a: GridCoord, b: GridCoord) -> f32 {
    let dx = (a.x - b.x).unsigned_abs() as f32;
    let dy = (a.y - b.y).unsigned_abs() as f32;
    let diagonal = dx.min(dy);
    let straight = dx.max(dy) - diagonal;
    DIAGONAL_COST * diagonal + ORTHOGONAL_COST * straight
}

/// A floor on the A* open set, ordered so a [`BinaryHeap`] (a max-heap)
/// yields the **lowest** `f_score` first.
#[derive(Debug, Clone, Copy)]
struct Frontier {
    f_score: f32,
    node: NavNode,
    height: f32,
}

impl PartialEq for Frontier {
    fn eq(&self, other: &Self) -> bool {
        self.f_score.total_cmp(&other.f_score) == Ordering::Equal
    }
}

impl Eq for Frontier {}

impl Ord for Frontier {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: smaller f_score is "greater", so it pops first.
        other.f_score.total_cmp(&self.f_score)
    }
}

impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn reconstruct(came_from: &HashMap<NavNode, NavNode>, mut current: NavNode) -> Vec<NavNode> {
    let mut path = vec![current];
    while let Some(&previous) = came_from.get(&current) {
        path.push(previous);
        current = previous;
    }
    path.reverse();
    path
}

/// A* on `grid` from `start` to `goal`, 8-directional. Diagonal moves cost
/// `sqrt(2)` and are disallowed when both shared orthogonal cells are
/// blocked (so a path never slips through the corner between two
/// diagonally-touching obstacles).
///
/// Returns the cell sequence from `start` to `goal` inclusive, or `None`
/// if either endpoint is unwalkable or no route exists. `start == goal`
/// (and walkable) returns `[start]`. Starts and ends on the lowest floor;
/// see [`find_node_path`] for the others.
pub fn find_path(grid: &NavGrid, start: GridCoord, goal: GridCoord) -> Option<Vec<GridCoord>> {
    let path = find_node_path(grid, NavNode::ground(start), NavNode::ground(goal))?;
    Some(path.into_iter().map(|node| node.coord).collect())
}

/// [`find_path`] between two floors. A step to a neighbouring cell lands
/// on its floor closest in height, within the grid's climb.
pub fn find_node_path(grid: &NavGrid, start: NavNode, goal: NavNode) -> Option<Vec<NavNode>> {
    if !grid.is_node_walkable(start) || !grid.is_node_walkable(goal) {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }
    let start_height = grid.node_height(start)?;

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<NavNode, f32> = HashMap::new();
    let mut came_from: HashMap<NavNode, NavNode> = HashMap::new();

    g_score.insert(start, 0.0);
    open.push(Frontier {
        f_score: octile_heuristic(start.coord, goal.coord),
        node: start,
        height: start_height,
    });

    while let Some(Frontier {
        node: current,
        height,
        ..
    }) = open.pop()
    {
        if current == goal {
            return Some(reconstruct(&came_from, current));
        }
        let current_g = g_score.get(&current).copied().unwrap_or(f32::INFINITY);

        for (next, next_height, diagonal) in neighbours(grid, current.coord, height) {
            let step_cost = if diagonal {
                DIAGONAL_COST
            } else {
                ORTHOGONAL_COST
            };
            let tentative_g = current_g + step_cost;
            if tentative_g < g_score.get(&next).copied().unwrap_or(f32::INFINITY) {
                came_from.insert(next, current);
                g_score.insert(next, tentative_g);
                open.push(Frontier {
                    f_score: tentative_g + octile_heuristic(next.coord, goal.coord),
                    node: next,
                    height: next_height,
                });
            }
        }
    }
    None
}

/// The floors reachable in one step from a floor of `coord` at `height`,
/// with their heights and whether the step is diagonal.
fn neighbours(
    grid: &NavGrid,
    coord: GridCoord,
    height: f32,
) -> impl Iterator<Item = (NavNode, f32, bool)> + '_ {
    NEIGHBOR_OFFSETS
        .iter()
        .enumerate()
        .filter_map(move |(i, &(dx, dy))| {
            let (next, next_height) =
                grid.step_to(height, GridCoord::new(coord.x + dx, coord.y + dy))?;
            let diagonal = i >= 4;
            if diagonal
                && (grid
                    .step_to(height, GridCoord::new(coord.x + dx, coord.y))
                    .is_none()
                    || grid
                        .step_to(height, GridCoord::new(coord.x, coord.y + dy))
                        .is_none())
            {
                return None;
            }
            Some((next, next_height, diagonal))
        })
}

/// [`find_path`] between two world positions, returning the route as
/// world-space cell centres. `start`/`goal` are `(x, z)`.
pub fn find_path_world(grid: &NavGrid, start: Vec2, goal: Vec2) -> Option<Vec<Vec2>> {
    let path = find_path(grid, grid.world_to_coord(start), grid.world_to_coord(goal))?;
    Some(
        path.into_iter()
            .map(|coord| grid.coord_to_world_center(coord))
            .collect(),
    )
}

/// How many rings of cells [`find_route`] searches for a walkable cell
/// when a route starts or ends in a blocked one.
const ROUTE_SNAP_RINGS: i32 = 4;

/// A walkable route between two world positions (`(x, z)`), ready to walk:
/// A* on the grid, string-pulled with [`smooth_path`], starting at
/// `start` itself and ending at `goal` itself.
///
/// An endpoint in a blocked cell snaps to the nearest walkable cell a few
/// cells away; past that, or with no route at all, the result is `None`.
/// When the goal snapped, the route ends at that cell's centre instead,
/// since `goal` itself is not reachable. Both ends stand on the floor
/// under them; see [`find_layered_route`] to give their heights.
pub fn find_route(grid: &NavGrid, start: Vec2, goal: Vec2) -> Option<Vec<Vec2>> {
    let lift = |at: Vec2| Vec3::new(at.x, grid.height_at(at), at.y);
    let route = find_layered_route(grid, lift(start), lift(goal))?;
    Some(route.into_iter().map(|p| Vec2::new(p.x, p.z)).collect())
}

/// [`find_route`] between two points given as `(x, height, z)`: each end
/// stands on the floor of its cell at its height, and the route's corners
/// carry the height of the floor they are on.
pub fn find_layered_route(grid: &NavGrid, start: Vec3, goal: Vec3) -> Option<Vec<Vec3>> {
    let flat = |p: Vec3| Vec2::new(p.x, p.z);
    let start_node = grid.nearest_walkable_node(flat(start), start.y, ROUTE_SNAP_RINGS)?;
    let goal_at = grid.node_at(flat(goal), goal.y);
    let goal_node = grid.nearest_walkable_node(flat(goal), goal.y, ROUTE_SNAP_RINGS)?;
    let nodes = smooth_node_path(grid, &find_node_path(grid, start_node, goal_node)?);
    let corner = |node: NavNode| {
        let centre = grid.coord_to_world_center(node.coord);
        Vec3::new(centre.x, grid.node_height(node).unwrap_or(0.0), centre.y)
    };
    let mut route: Vec<Vec3> = nodes.iter().skip(1).map(|&node| corner(node)).collect();
    let end = if goal_node == goal_at {
        goal
    } else {
        corner(goal_node)
    };
    match route.last_mut() {
        Some(last) => *last = end,
        None => route.push(end),
    }
    Some(route)
}

/// Whether every cell the straight segment from `a` to `b` passes through
/// is walkable. A diagonal step that would squeeze between two
/// diagonally-touching blocked cells counts as no line of sight.
/// Symmetric in `a` and `b`. Both ends are on the lowest floor.
pub fn line_of_sight(grid: &NavGrid, a: GridCoord, b: GridCoord) -> bool {
    node_line_of_sight(grid, NavNode::ground(a), NavNode::ground(b))
}

/// [`line_of_sight`] between two floors: the segment walks from floor to
/// floor within the grid's climb and must arrive on `b`'s.
pub fn node_line_of_sight(grid: &NavGrid, a: NavNode, b: NavNode) -> bool {
    let Some((mut height, false)) = grid.span(a) else {
        return false;
    };
    let mut current = a;
    let mut x = a.coord.x;
    let mut y = a.coord.y;
    let dx = (b.coord.x - a.coord.x).abs();
    let dy = (b.coord.y - a.coord.y).abs();
    let step_x = if a.coord.x < b.coord.x { 1 } else { -1 };
    let step_y = if a.coord.y < b.coord.y { 1 } else { -1 };
    let mut error = dx - dy;

    loop {
        if x == b.coord.x && y == b.coord.y {
            return current == b;
        }
        let double_error = 2 * error;
        let advance_x = double_error > -dy;
        let advance_y = double_error < dx;
        if advance_x && advance_y {
            // Diagonal move — reject if it clips a blocked corner.
            if grid
                .step_to(height, GridCoord::new(x + step_x, y))
                .is_none()
                || grid
                    .step_to(height, GridCoord::new(x, y + step_y))
                    .is_none()
            {
                return false;
            }
            error += dx - dy;
            x += step_x;
            y += step_y;
        } else if advance_x {
            error -= dy;
            x += step_x;
        } else {
            error += dx;
            y += step_y;
        }
        match grid.step_to(height, GridCoord::new(x, y)) {
            Some((node, next_height)) => {
                current = node;
                height = next_height;
            }
            None => return false,
        }
    }
}

/// String-pulls `path` (a [`find_path`] result): keeps a waypoint only
/// when the anchor before it can't see straight through to the waypoint
/// after it. The endpoints are always kept; a path of two or fewer cells
/// is returned unchanged.
pub fn smooth_path(grid: &NavGrid, path: &[GridCoord]) -> Vec<GridCoord> {
    let nodes: Vec<NavNode> = path.iter().map(|coord| NavNode::ground(*coord)).collect();
    smooth_node_path(grid, &nodes)
        .into_iter()
        .map(|node| node.coord)
        .collect()
}

/// [`smooth_path`] for a [`find_node_path`] result.
pub fn smooth_node_path(grid: &NavGrid, path: &[NavNode]) -> Vec<NavNode> {
    if path.len() <= 2 {
        return path.to_vec();
    }
    let mut result = vec![path[0]];
    let mut anchor = 0usize;
    for i in 1..path.len() - 1 {
        if !node_line_of_sight(grid, path[anchor], path[i + 1]) {
            result.push(path[i]);
            anchor = i;
        }
    }
    result.push(path[path.len() - 1]);
    result
}

/// Advances `position` along `path` by up to `speed * dt` world units this
/// step, consuming every waypoint reached along the way. Returns the new
/// position and the index of the first waypoint **not** yet reached
/// (`path.len()` once the route is finished).
///
/// A non-positive or non-finite `dt`/`speed`, or an empty `path`, returns
/// `(position, 0)` unchanged.
pub fn follow_path(position: Vec2, path: &[Vec2], speed: f32, dt: f32) -> (Vec2, usize) {
    if path.is_empty() || dt <= 0.0 || !dt.is_finite() || speed <= 0.0 || !speed.is_finite() {
        return (position, 0);
    }

    let mut current = position;
    let mut budget = speed * dt;
    let mut index = 0usize;
    while index < path.len() {
        let to_target = path[index] - current;
        let distance = to_target.length();
        if distance <= budget {
            current = path[index];
            budget -= distance;
            index += 1;
        } else {
            current += to_target / distance * budget;
            break;
        }
    }
    (current, index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: u32, height: u32) -> NavGrid {
        NavGrid::new(width, height, 1.0, Vec2::ZERO).expect("valid dimensions")
    }

    fn path_cost(path: &[GridCoord]) -> f32 {
        path.windows(2)
            .map(|pair| {
                let dx = (pair[1].x - pair[0].x).abs();
                let dy = (pair[1].y - pair[0].y).abs();
                if dx == 1 && dy == 1 {
                    DIAGONAL_COST
                } else {
                    ORTHOGONAL_COST * (dx + dy) as f32
                }
            })
            .sum()
    }

    #[test]
    fn new_rejects_zero_dimensions() {
        assert!(matches!(
            NavGrid::new(0, 4, 1.0, Vec2::ZERO),
            Err(NavError::InvalidDimensions { .. })
        ));
        assert!(NavGrid::new(4, 0, 1.0, Vec2::ZERO).is_err());
    }

    #[test]
    fn new_rejects_bad_cell_size() {
        assert!(matches!(
            NavGrid::new(4, 4, 0.0, Vec2::ZERO),
            Err(NavError::InvalidCellSize { .. })
        ));
        assert!(NavGrid::new(4, 4, -1.0, Vec2::ZERO).is_err());
        assert!(NavGrid::new(4, 4, f32::NAN, Vec2::ZERO).is_err());
    }

    #[test]
    fn new_rejects_a_non_finite_origin() {
        assert!(NavGrid::new(4, 4, 1.0, Vec2::new(f32::NAN, 0.0)).is_err());
        assert!(NavGrid::new(4, 4, 1.0, Vec2::new(0.0, f32::INFINITY)).is_err());
    }

    #[test]
    fn new_rejects_oversized_grids() {
        assert!(matches!(
            NavGrid::new(50_000, 50_000, 1.0, Vec2::ZERO),
            Err(NavError::GridTooLarge { .. })
        ));
    }

    #[test]
    fn world_and_coord_round_trip_within_a_cell() {
        let grid = NavGrid::new(10, 10, 2.0, Vec2::new(-5.0, 3.0)).unwrap();
        let coord = GridCoord::new(4, 7);
        let center = grid.coord_to_world_center(coord);
        assert_eq!(grid.world_to_coord(center), coord);
        // A point near the cell's corner still maps back to it.
        assert_eq!(grid.world_to_coord(center + Vec2::splat(0.9)), coord);
    }

    #[test]
    fn out_of_bounds_coords_are_blocked_but_not_walkable() {
        let grid = grid(4, 4);
        let outside = GridCoord::new(-1, 0);
        assert!(grid.is_blocked(outside));
        assert!(!grid.is_walkable(outside));
        assert!(!grid.in_bounds(outside));
    }

    #[test]
    fn block_aabb_marks_the_overlapping_cells_only() {
        let mut grid = grid(6, 6);
        grid.block_aabb(Vec2::new(1.5, 1.5), Vec2::new(3.5, 2.5));
        assert!(grid.is_blocked(GridCoord::new(1, 1)));
        assert!(grid.is_blocked(GridCoord::new(3, 2)));
        assert!(grid.is_walkable(GridCoord::new(0, 0)));
        assert!(grid.is_walkable(GridCoord::new(4, 4)));
    }

    #[test]
    fn block_aabb_outside_the_grid_blocks_nothing() {
        let mut grid = grid(4, 4);
        grid.block_aabb(Vec2::new(100.0, 100.0), Vec2::new(200.0, 200.0));
        assert!(grid.blocked.iter().all(|&b| !b));
    }

    #[test]
    fn block_circle_marks_cells_within_the_radius() {
        let mut grid = grid(9, 9);
        grid.block_circle(Vec2::new(4.5, 4.5), 1.6);
        assert!(grid.is_blocked(GridCoord::new(4, 4)));
        assert!(grid.is_blocked(GridCoord::new(3, 4)));
        assert!(grid.is_walkable(GridCoord::new(4, 6)));
        assert!(grid.is_walkable(GridCoord::new(0, 0)));
    }

    #[test]
    fn block_circle_ignores_bad_radius() {
        let mut grid = grid(4, 4);
        grid.block_circle(Vec2::splat(2.0), 0.0);
        grid.block_circle(Vec2::splat(2.0), -1.0);
        grid.block_circle(Vec2::splat(2.0), f32::INFINITY);
        assert!(grid.blocked.iter().all(|&b| !b));
    }

    #[test]
    fn find_path_on_an_empty_grid_is_a_straight_line() {
        let grid = grid(10, 10);
        let path = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(5, 0)).unwrap();
        assert_eq!(path.first(), Some(&GridCoord::new(0, 0)));
        assert_eq!(path.last(), Some(&GridCoord::new(5, 0)));
        assert!((path_cost(&path) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn find_path_equal_start_and_goal_is_a_single_cell() {
        let grid = grid(4, 4);
        assert_eq!(
            find_path(&grid, GridCoord::new(2, 2), GridCoord::new(2, 2)),
            Some(vec![GridCoord::new(2, 2)])
        );
    }

    #[test]
    fn find_path_rejects_unwalkable_endpoints() {
        let mut grid = grid(4, 4);
        grid.set_blocked(GridCoord::new(3, 3), true);
        assert_eq!(
            find_path(&grid, GridCoord::new(0, 0), GridCoord::new(3, 3)),
            None
        );
        assert_eq!(
            find_path(&grid, GridCoord::new(3, 3), GridCoord::new(0, 0)),
            None
        );
    }

    #[test]
    fn find_path_routes_around_a_wall() {
        // A vertical wall at x = 2 spanning y = 0..=3, with a gap at y = 4.
        let mut grid = grid(6, 6);
        for y in 0..4 {
            grid.set_blocked(GridCoord::new(2, y), true);
        }
        let path = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(5, 0)).unwrap();
        // Must detour to y >= 4 to get around.
        assert!(path.iter().any(|c| c.y >= 4));
        // And it must not step onto a wall cell.
        assert!(path.iter().all(|c| !grid.is_blocked(*c)));
    }

    #[test]
    fn find_path_returns_none_when_the_goal_is_walled_off() {
        let mut grid = grid(6, 6);
        // Box the goal cell in completely.
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            grid.set_blocked(GridCoord::new(3 + dx, 3 + dy), true);
        }
        assert_eq!(
            find_path(&grid, GridCoord::new(0, 0), GridCoord::new(3, 3)),
            None
        );
    }

    #[test]
    fn find_path_does_not_cut_the_corner_between_two_blockers() {
        // Blockers at (2,1) and (1,2) flank the (1,1)->(2,2) diagonal; a
        // naive 8-way search would slip through that corner. A detour
        // still exists, so the path must exist AND avoid that step.
        let mut grid = grid(5, 5);
        grid.set_blocked(GridCoord::new(2, 1), true);
        grid.set_blocked(GridCoord::new(1, 2), true);
        let path = find_path(&grid, GridCoord::new(1, 1), GridCoord::new(3, 3)).unwrap();
        assert!(
            !path
                .windows(2)
                .any(|w| w[0] == GridCoord::new(1, 1) && w[1] == GridCoord::new(2, 2))
        );
        assert!(path.iter().all(|c| !grid.is_blocked(*c)));
    }

    #[test]
    fn heuristic_never_exceeds_the_real_path_cost() {
        let grid = grid(12, 12);
        for &(sx, sy, gx, gy) in &[(0, 0, 5, 0), (0, 0, 4, 4), (1, 2, 9, 7), (3, 8, 10, 1)] {
            let start = GridCoord::new(sx, sy);
            let goal = GridCoord::new(gx, gy);
            let cost = path_cost(&find_path(&grid, start, goal).unwrap());
            assert!(octile_heuristic(start, goal) <= cost + 1e-4);
        }
    }

    #[test]
    fn line_of_sight_is_clear_on_an_empty_grid_and_symmetric() {
        let grid = grid(10, 10);
        let a = GridCoord::new(1, 1);
        let b = GridCoord::new(8, 5);
        assert!(line_of_sight(&grid, a, b));
        assert!(line_of_sight(&grid, b, a));
    }

    #[test]
    fn line_of_sight_is_blocked_by_a_cell_on_the_segment() {
        let mut grid = grid(10, 10);
        grid.set_blocked(GridCoord::new(4, 2), true);
        assert!(!line_of_sight(
            &grid,
            GridCoord::new(0, 0),
            GridCoord::new(8, 4)
        ));
    }

    #[test]
    fn line_of_sight_does_not_cut_past_one_blocked_corner_side() {
        let mut grid = grid(3, 3);
        grid.set_blocked(GridCoord::new(1, 0), true);

        assert!(!line_of_sight(
            &grid,
            GridCoord::new(0, 0),
            GridCoord::new(2, 2)
        ));
    }

    #[test]
    fn smooth_path_collapses_a_staircase_on_an_empty_grid() {
        let grid = grid(10, 10);
        let raw = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(6, 3)).unwrap();
        let smoothed = smooth_path(&grid, &raw);
        assert_eq!(smoothed, vec![GridCoord::new(0, 0), GridCoord::new(6, 3)]);
    }

    #[test]
    fn smooth_path_keeps_the_corner_around_a_wall() {
        let mut grid = grid(8, 8);
        for y in 0..5 {
            grid.set_blocked(GridCoord::new(3, y), true);
        }
        let raw = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(6, 0)).unwrap();
        let smoothed = smooth_path(&grid, &raw);
        assert_eq!(smoothed.first(), raw.first());
        assert_eq!(smoothed.last(), raw.last());
        // Some corner near the wall survives the pull.
        assert!(smoothed.len() >= 3);
        assert!(smoothed.iter().all(|c| !grid.is_blocked(*c)));
    }

    #[test]
    fn smooth_path_passes_short_paths_through_unchanged() {
        let grid = grid(4, 4);
        assert_eq!(smooth_path(&grid, &[]), Vec::<GridCoord>::new());
        let one = vec![GridCoord::new(1, 1)];
        assert_eq!(smooth_path(&grid, &one), one);
        let two = vec![GridCoord::new(0, 0), GridCoord::new(1, 1)];
        assert_eq!(smooth_path(&grid, &two), two);
    }

    #[test]
    fn find_path_world_returns_cell_centres() {
        let grid = NavGrid::new(10, 10, 2.0, Vec2::ZERO).unwrap();
        let path = find_path_world(&grid, Vec2::new(1.0, 1.0), Vec2::new(9.0, 1.0)).unwrap();
        assert_eq!(path.first(), Some(&Vec2::new(1.0, 1.0)));
        assert_eq!(path.last(), Some(&Vec2::new(9.0, 1.0)));
    }

    #[test]
    fn follow_path_moves_toward_the_next_waypoint() {
        let path = [Vec2::new(10.0, 0.0)];
        let (pos, index) = follow_path(Vec2::ZERO, &path, 3.0, 1.0);
        assert_eq!(pos, Vec2::new(3.0, 0.0));
        assert_eq!(index, 0);
    }

    #[test]
    fn follow_path_consumes_multiple_waypoints_in_one_big_step() {
        let path = [
            Vec2::new(1.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(3.0, 0.0),
        ];
        let (pos, index) = follow_path(Vec2::ZERO, &path, 100.0, 1.0);
        assert_eq!(pos, Vec2::new(3.0, 0.0));
        assert_eq!(index, 3);
    }

    #[test]
    fn follow_path_stops_exactly_at_the_end() {
        let path = [Vec2::new(2.0, 0.0), Vec2::new(2.0, 2.0)];
        let (pos, index) = follow_path(Vec2::new(2.0, 1.5), &path, 10.0, 1.0);
        assert_eq!(pos, Vec2::new(2.0, 2.0));
        assert_eq!(index, 2);
    }

    #[test]
    fn follow_path_is_a_noop_for_bad_inputs() {
        let path = [Vec2::new(5.0, 5.0)];
        assert_eq!(follow_path(Vec2::ONE, &path, 3.0, 0.0), (Vec2::ONE, 0));
        assert_eq!(follow_path(Vec2::ONE, &path, 0.0, 1.0), (Vec2::ONE, 0));
        assert_eq!(follow_path(Vec2::ONE, &[], 3.0, 1.0), (Vec2::ONE, 0));
        assert_eq!(follow_path(Vec2::ONE, &path, 3.0, f32::NAN), (Vec2::ONE, 0));
    }

    #[test]
    fn heights_follow_the_cell_and_clamp_at_the_edge() {
        let hills = NavGrid::new(2, 1, 1.0, Vec2::ZERO)
            .and_then(|grid| grid.with_heights(vec![1.0, 3.0]))
            .expect("valid heights");
        assert_eq!(hills.height_at(Vec2::new(0.5, 0.5)), 1.0);
        assert_eq!(hills.height_at(Vec2::new(1.5, 0.5)), 3.0);
        assert_eq!(hills.height_at(Vec2::new(9.0, -9.0)), 3.0);
        assert_eq!(hills.height_at(Vec2::new(-9.0, 0.5)), 1.0);
        assert_eq!(grid(2, 2).height_at(Vec2::ONE), 0.0);
    }

    #[test]
    fn with_heights_rejects_wrong_counts_and_nan() {
        assert!(matches!(
            grid(2, 2).with_heights(vec![0.0; 3]),
            Err(NavError::InvalidHeights { count: 3, cells: 4 })
        ));
        assert!(grid(1, 1).with_heights(vec![f32::NAN]).is_err());
        assert!(grid(1, 1).with_heights(Vec::new()).is_ok());
    }

    #[test]
    fn nearest_walkable_finds_the_closest_free_cell() {
        let mut grid = grid(5, 5);
        grid.block_aabb(Vec2::new(1.0, 1.0), Vec2::new(3.9, 3.9));
        assert_eq!(
            grid.nearest_walkable(GridCoord::new(0, 0), 2),
            Some(GridCoord::new(0, 0))
        );
        let near = grid
            .nearest_walkable(GridCoord::new(2, 2), 2)
            .expect("free ring");
        assert!(grid.is_walkable(near));
        assert_eq!((near.x - 2).abs().max((near.y - 2).abs()), 2);
        assert_eq!(grid.nearest_walkable(GridCoord::new(2, 2), 1), None);
    }

    #[test]
    fn find_route_starts_after_the_agent_and_ends_on_the_goal() {
        let mut grid = grid(10, 10);
        grid.block_aabb(Vec2::new(5.0, 0.0), Vec2::new(5.9, 7.9));
        let goal = Vec2::new(8.3, 1.2);
        let route = find_route(&grid, Vec2::new(1.5, 1.5), goal).expect("route");
        assert_eq!(route.last().copied(), Some(goal));
        // Around the wall, so at least one corner before the goal.
        assert!(route.len() >= 2);
        assert!(route.iter().any(|point| point.y > 7.9));
        assert_eq!(
            find_route(&grid, Vec2::new(1.5, 1.5), Vec2::new(2.5, 1.5)),
            Some(vec![Vec2::new(2.5, 1.5)])
        );
    }

    #[test]
    fn find_route_snaps_a_blocked_goal_to_the_nearest_cell() {
        let mut grid = grid(6, 1);
        grid.set_blocked(GridCoord::new(5, 0), true);
        let route = find_route(&grid, Vec2::new(0.5, 0.5), Vec2::new(5.5, 0.5)).expect("route");
        assert_eq!(route.last().copied(), Some(Vec2::new(4.5, 0.5)));
    }

    /// A 9x3 grid of ground at 0 with a gap in columns 3..=5 (blocked), and
    /// a bridge at height 2 over columns 2..=6 of the middle row, reached by
    /// ramps of 1 per cell from columns 0 and 8... built as floors above
    /// the ground where both exist.
    fn bridge_grid() -> NavGrid {
        let mut grid = NavGrid::new(9, 3, 1.0, Vec2::ZERO).expect("grid");
        let mut heights = vec![0.0; 27];
        for row in 0..3 {
            for column in 3..=5 {
                grid.set_blocked(GridCoord::new(column, row), true);
            }
        }
        // The middle row climbs 0, 1, 2 and holds 2 across the gap.
        let deck = [0.0, 1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 1.0, 0.0];
        let mut upper = Vec::new();
        for (column, height) in deck.iter().enumerate() {
            let cell = (9 + column) as u32;
            if (3..=5).contains(&column) {
                // No ground under the deck: the deck is the cell's only floor.
                grid.set_blocked(GridCoord::new(column as i32, 1), false);
                heights[9 + column] = *height;
            } else if *height > 0.0 {
                // Ground under the ramp, blocked by it; the ramp above.
                grid.set_blocked(GridCoord::new(column as i32, 1), true);
                upper.push(Span {
                    cell,
                    height: *height,
                    blocked: false,
                });
            }
        }
        grid.with_heights(heights)
            .and_then(|grid| grid.with_layers(upper, Some(1.0)))
            .expect("valid layers")
    }

    #[test]
    fn a_route_crosses_a_bridge_on_its_upper_floor() {
        let grid = bridge_grid();
        assert_eq!(grid.layer_count(GridCoord::new(1, 1)), 2);
        let path = find_node_path(
            &grid,
            NavNode::ground(GridCoord::new(0, 0)),
            NavNode::ground(GridCoord::new(8, 0)),
        )
        .expect("over the bridge");
        assert!(path.iter().any(|node| node.coord == GridCoord::new(4, 1)));
        assert!(
            path.iter()
                .any(|node| node.coord == GridCoord::new(1, 1) && node.layer == 1),
            "climbs the ramp on its upper floor: {path:?}"
        );
        let route = find_layered_route(&grid, Vec3::new(0.5, 0.0, 0.5), Vec3::new(8.5, 0.0, 0.5))
            .expect("route");
        assert_eq!(route.last().copied(), Some(Vec3::new(8.5, 0.0, 0.5)));
        let (from, to) = (route[route.len() - 2], route[route.len() - 1]);
        let a = grid.node_at(Vec2::new(from.x, from.z), from.y);
        let b = grid.node_at(Vec2::new(to.x, to.z), to.y);
        assert!(
            node_line_of_sight(&grid, a, b),
            "each leg is walkable: {route:?}"
        );
    }

    #[test]
    fn floors_too_far_apart_do_not_connect() {
        let mut grid = NavGrid::new(3, 1, 1.0, Vec2::ZERO)
            .and_then(|grid| grid.with_heights(vec![0.0, 3.0, 0.0]))
            .and_then(|grid| grid.with_layers(Vec::new(), Some(0.5)))
            .expect("grid");
        assert!(find_path(&grid, GridCoord::new(0, 0), GridCoord::new(2, 0)).is_none());
        grid = grid.with_layers(Vec::new(), None).expect("grid");
        assert!(
            find_path(&grid, GridCoord::new(0, 0), GridCoord::new(2, 0)).is_some(),
            "without a climb heights are ignored"
        );
    }

    #[test]
    fn node_at_picks_the_floor_stood_on() {
        let grid = bridge_grid();
        let ramp = Vec2::new(2.5, 1.5);
        assert_eq!(grid.node_at(ramp, 0.0).layer, 0, "under the ramp");
        assert_eq!(grid.node_at(ramp, 2.0).layer, 1, "on the ramp");
        assert_eq!(grid.floor_height(ramp, 2.1), 2.0);
        assert_eq!(grid.floor_height(ramp, 0.0), 0.0);
    }

    #[test]
    fn with_layers_rejects_bad_floors() {
        let base = || NavGrid::new(2, 1, 1.0, Vec2::ZERO).expect("grid");
        let span = |cell, height| Span {
            cell,
            height,
            blocked: false,
        };
        assert!(base().with_layers(vec![span(2, 1.0)], None).is_err());
        assert!(base().with_layers(vec![span(0, f32::NAN)], None).is_err());
        assert!(
            base()
                .with_layers(vec![span(1, 1.0), span(0, 1.0)], None)
                .is_err()
        );
        let crowded = (1..=MAX_LAYERS).map(|h| span(0, h as f32)).collect();
        assert!(base().with_layers(crowded, None).is_err());
        assert!(base().with_layers(Vec::new(), Some(-1.0)).is_err());
        let fine = vec![span(0, 1.0), span(0, 2.0), span(1, 5.0)];
        let grid = base().with_layers(fine, Some(1.0)).expect("valid");
        assert_eq!(grid.layer_count(GridCoord::new(0, 0)), 3);
        assert_eq!(grid.floors().count(), 5);
    }

    #[test]
    fn moving_solids_block_floors_until_they_leave() {
        let mut grid = grid(6, 1);
        let clearance = Clearance {
            radius: 0.0,
            step: 0.2,
            height: 1.8,
        };
        let crate_at = |x: f32| Solid::aabb(Vec3::new(x, 0.0, 0.1), Vec3::new(x + 0.8, 1.0, 0.9));
        assert!(grid.block_moving(&[crate_at(2.1)], &clearance));
        assert!(grid.is_blocked(GridCoord::new(2, 0)));
        assert!(find_path(&grid, GridCoord::new(0, 0), GridCoord::new(5, 0)).is_none());
        assert!(
            !grid.block_moving(&[crate_at(2.1)], &clearance),
            "unchanged"
        );
        assert!(grid.block_moving(&[], &clearance));
        assert!(find_path(&grid, GridCoord::new(0, 0), GridCoord::new(5, 0)).is_some());
    }

    #[test]
    fn nav_error_displays() {
        let err = NavError::InvalidDimensions {
            width: 0,
            height: 4,
        };
        assert!(err.to_string().contains("nonzero"));
    }
}
