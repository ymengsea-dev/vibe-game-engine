//! # engine_ai
//!
//! Gameplay AI. First subsystem: **navigation and pathfinding** — a
//! walkable [`NavGrid`] over the ground plane, [`find_path`] (A*, 8-way,
//! octile heuristic, no corner-cutting), grid [`line_of_sight`],
//! string-pulling [`smooth_path`], and a pure [`follow_path`] step for
//! moving an agent along a route.
//!
//! ## Status
//!
//! Navigation grid + A* pathfinding implemented ([`nav`]). All pure CPU
//! math — no `wgpu`, no `bevy_ecs`. A caller builds a [`NavGrid`] from
//! whatever world knowledge it has (block AABBs / circles / individual
//! cells), queries [`find_path`] / [`find_path_world`], optionally
//! [`smooth_path`]s the result, and advances an agent with [`follow_path`].
//!
//! Grids can carry a ground height per cell ([`NavGrid::with_heights`]),
//! and [`find_route`] is the one-call route a walking agent uses. Studio
//! bakes grids from colliders and terrain slope; `engine::gameplay` walks
//! agents along them, and [`avoid`] steers walking agents round each
//! other. Polygon navmeshes are future work.

pub mod avoid;
pub mod nav;

pub use avoid::{AVOID_HORIZON, AvoidAgent, avoid};

pub use nav::{
    GridCoord, NavError, NavGrid, find_path, find_path_world, find_route, follow_path,
    line_of_sight, smooth_path,
};
