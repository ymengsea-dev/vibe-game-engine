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

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::Vec2;

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

/// A uniform grid of walkable / blocked cells over the world's XZ plane.
#[derive(Debug, Clone, PartialEq)]
pub struct NavGrid {
    width: u32,
    height: u32,
    cell_size: f32,
    origin: Vec2,
    /// Row-major, `width * height`. `true` = blocked.
    blocked: Vec<bool>,
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
        })
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
    pub fn is_blocked(&self, coord: GridCoord) -> bool {
        match self.index(coord) {
            Some(index) => self.blocked[index],
            None => true,
        }
    }

    /// Whether `coord` is in bounds and not blocked.
    pub fn is_walkable(&self, coord: GridCoord) -> bool {
        self.index(coord).is_some_and(|index| !self.blocked[index])
    }

    /// Sets `coord`'s blocked state. Out-of-bounds coordinates are
    /// ignored.
    pub fn set_blocked(&mut self, coord: GridCoord, blocked: bool) {
        if let Some(index) = self.index(coord) {
            self.blocked[index] = blocked;
        }
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

/// A cell on the A* open set, ordered so a [`BinaryHeap`] (a max-heap)
/// yields the **lowest** `f_score` first.
#[derive(Debug, Clone, Copy)]
struct Frontier {
    f_score: f32,
    coord: GridCoord,
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

fn reconstruct(
    came_from: &HashMap<GridCoord, GridCoord>,
    mut current: GridCoord,
) -> Vec<GridCoord> {
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
/// (and walkable) returns `[start]`.
pub fn find_path(grid: &NavGrid, start: GridCoord, goal: GridCoord) -> Option<Vec<GridCoord>> {
    if !grid.is_walkable(start) || !grid.is_walkable(goal) {
        return None;
    }
    if start == goal {
        return Some(vec![start]);
    }

    let mut open = BinaryHeap::new();
    let mut g_score: HashMap<GridCoord, f32> = HashMap::new();
    let mut came_from: HashMap<GridCoord, GridCoord> = HashMap::new();

    g_score.insert(start, 0.0);
    open.push(Frontier {
        f_score: octile_heuristic(start, goal),
        coord: start,
    });

    while let Some(Frontier { coord: current, .. }) = open.pop() {
        if current == goal {
            return Some(reconstruct(&came_from, current));
        }
        let current_g = g_score.get(&current).copied().unwrap_or(f32::INFINITY);

        for (i, &(dx, dy)) in NEIGHBOR_OFFSETS.iter().enumerate() {
            let next = GridCoord {
                x: current.x + dx,
                y: current.y + dy,
            };
            if !grid.is_walkable(next) {
                continue;
            }
            let diagonal = i >= 4;
            if diagonal {
                let side_a = GridCoord {
                    x: current.x + dx,
                    y: current.y,
                };
                let side_b = GridCoord {
                    x: current.x,
                    y: current.y + dy,
                };
                if grid.is_blocked(side_a) || grid.is_blocked(side_b) {
                    continue;
                }
            }
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
                    f_score: tentative_g + octile_heuristic(next, goal),
                    coord: next,
                });
            }
        }
    }
    None
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

/// Whether every cell the straight segment from `a` to `b` passes through
/// is walkable. A diagonal step that would squeeze between two
/// diagonally-touching blocked cells counts as no line of sight.
/// Symmetric in `a` and `b`.
pub fn line_of_sight(grid: &NavGrid, a: GridCoord, b: GridCoord) -> bool {
    let mut x = a.x;
    let mut y = a.y;
    let dx = (b.x - a.x).abs();
    let dy = (b.y - a.y).abs();
    let step_x = if a.x < b.x { 1 } else { -1 };
    let step_y = if a.y < b.y { 1 } else { -1 };
    let mut error = dx - dy;

    loop {
        if grid.is_blocked(GridCoord { x, y }) {
            return false;
        }
        if x == b.x && y == b.y {
            return true;
        }
        let double_error = 2 * error;
        let advance_x = double_error > -dy;
        let advance_y = double_error < dx;
        if advance_x && advance_y {
            // Diagonal move — reject if it clips a blocked corner.
            if grid.is_blocked(GridCoord { x: x + step_x, y })
                && grid.is_blocked(GridCoord { x, y: y + step_y })
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
    }
}

/// String-pulls `path` (a [`find_path`] result): keeps a waypoint only
/// when the anchor before it can't see straight through to the waypoint
/// after it. The endpoints are always kept; a path of two or fewer cells
/// is returned unchanged.
pub fn smooth_path(grid: &NavGrid, path: &[GridCoord]) -> Vec<GridCoord> {
    if path.len() <= 2 {
        return path.to_vec();
    }
    let mut result = vec![path[0]];
    let mut anchor = 0usize;
    for i in 1..path.len() - 1 {
        if !line_of_sight(grid, path[anchor], path[i + 1]) {
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
    fn nav_error_displays() {
        let err = NavError::InvalidDimensions {
            width: 0,
            height: 4,
        };
        assert!(err.to_string().contains("nonzero"));
    }
}
