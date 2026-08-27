//! Frame stage labels: the named schedules an [`crate::Ecs`] always has.

use bevy_ecs::schedule::ScheduleLabel;

/// Gameplay/simulation systems: input handling, movement, AI, physics
/// stepping, ... — anything that changes world state for this frame.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Update;

/// Systems that read the post-`Update` world state and prepare it for
/// rendering (e.g. syncing ECS `Transform`/`Camera`/`MeshRenderer`
/// components into whatever the renderer consumes). Named "extract" after
/// the common ECS-renderer pattern of copying just the data a frame needs
/// out of the main world, rather than the renderer reading `World`
/// directly.
///
/// Runs after `Update`, so it sees this frame's simulation results, not
/// last frame's.
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderExtract;
