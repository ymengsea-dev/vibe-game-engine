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
//! Polygon navmeshes, a `NavAgent` ECS component + follow system, local
//! steering / obstacle avoidance, and building the grid automatically from
//! physics colliders or terrain slope are all future work.

pub mod nav;

pub use nav::{
    GridCoord, NavError, NavGrid, find_path, find_path_world, follow_path, line_of_sight,
    smooth_path,
};
