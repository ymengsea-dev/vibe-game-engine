//! In-game AI shared by Lua and Rust games: objects walking the scene's nav
//! grid, noticing targets, and brains deciding what they do.
//!
//! [`super::GameplayRuntime::update_rules`] runs one [`tick`] per frame:
//! 1. senses: every object with senses looks and listens for its target tag
//!    and hears noises;
//! 2. brains: Simple brains and behaviour trees choose where to walk and what
//!    to do, as routes and [`GameCommand`]s;
//! 3. walking: every object with a route takes this frame's step.
//!
//! What happened comes back as [`AiEvent`]s, which run the matching visual
//! rules and then go to scripts.

mod nav;
mod senses;
mod simple;
#[cfg(test)]
mod tests;
mod tree;

use engine_ecs::components::{GlobalTransform, Transform};
use engine_ecs::prelude::{Entity, World};
use engine_physics::PhysicsWorld;
use engine_scene::{AiState, SceneAi, SceneObjectId};
use engine_scripting::GameCommand;
use glam::{Quat, Vec3};

pub(super) use nav::{ActiveNavGrid, apply_route_commands, find_world_route, nav_grid};
pub(super) use senses::Noise;
pub(super) use senses::line_of_sight;

/// Something an object's AI did or noticed this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum AiEvent {
    /// It reached where it was walking to.
    Arrived {
        /// The object that arrived.
        object_id: String,
    },
    /// It noticed a target, by sight or hearing.
    TargetSeen {
        /// The object that noticed.
        object_id: String,
        /// The target it noticed.
        target_id: String,
    },
    /// It went `forget_after` seconds without seeing or hearing its target.
    TargetLost {
        /// The object that lost its target.
        object_id: String,
        /// The target it lost.
        target_id: String,
    },
    /// It heard a noise.
    NoiseHeard {
        /// The object that heard it.
        object_id: String,
        /// Where the noise was made.
        position: [f32; 3],
    },
    /// Its Simple brain moved to another state.
    StateChanged {
        /// The object whose state changed.
        object_id: String,
        /// The state it is in now: `idle`, `patrol`, `chase` or `attack`.
        state: &'static str,
    },
}

impl AiEvent {
    /// The object the event is about.
    pub fn object_id(&self) -> &str {
        match self {
            Self::Arrived { object_id }
            | Self::TargetSeen { object_id, .. }
            | Self::TargetLost { object_id, .. }
            | Self::NoiseHeard { object_id, .. }
            | Self::StateChanged { object_id, .. } => object_id,
        }
    }
}

/// What one object's senses and brain remember between frames.
#[derive(bevy_ecs::component::Component, Debug, Clone, Default)]
pub(super) struct AiMemory {
    /// The target it is after, by object ID.
    pub target: Option<String>,
    /// Whether it saw the target this frame.
    pub sees: bool,
    /// Whether it heard the target this frame.
    pub hears: bool,
    /// Seconds since it last saw or heard the target.
    pub since_contact: f32,
    /// Where it last saw or heard the target.
    pub last_known: Option<Vec3>,
    /// A noise it heard and has not checked yet.
    pub noise: Option<Vec3>,
    /// The Simple brain's state; `None` until its first frame.
    pub state: Option<AiState>,
    /// A state a script asked for, taken on the brain's next frame.
    pub forced: Option<AiState>,
    /// The patrol point it walks to or waits at.
    pub patrol_index: usize,
    /// Walking the patrol points backwards (back-and-forth patrols).
    pub patrol_reverse: bool,
    /// Walking to the patrol point at `patrol_index`.
    pub walking_patrol: bool,
    /// Walking to a noise it heard, or looking around once there.
    pub investigating: bool,
    /// Seconds left to wait before walking on.
    pub wait: f32,
    /// Seconds until the next attack may happen.
    pub cooldown: f32,
    /// Seconds until a chase works out a new route to the target.
    pub repath: f32,
    /// The behaviour tree's running state.
    pub tree: tree::TreeMemory,
}

/// What one AI frame produced.
#[derive(Debug, Default)]
pub(super) struct AiTick {
    /// Changes for the runtime to apply.
    pub commands: Vec<GameCommand>,
    /// What happened, in order.
    pub events: Vec<AiEvent>,
}

/// Runs senses, brains and walking for one frame of `dt` seconds.
pub(super) fn tick(
    world: &mut World,
    physics: &PhysicsWorld,
    noises: &[Noise],
    dt: f32,
    rng: &mut engine_utils::Rng,
) -> AiTick {
    let mut out = AiTick::default();
    nav::refresh_active_grid(world);
    senses::update(world, physics, noises, dt, &mut out);
    simple::update(world, dt, &mut out);
    tree::update(world, dt, rng, &mut out);
    nav::walk(world, physics, dt, &mut out);
    out
}

/// Forces a Simple brain into `state`, from `engine.set_ai_state` or
/// `GameCommand::SetAiState`. Its logic carries on from there.
pub(super) fn apply_state_commands(world: &mut World, commands: &[GameCommand]) {
    for command in commands {
        let GameCommand::SetAiState { object_id, state } = command else {
            continue;
        };
        let Some(state) = AiState::from_name(state) else {
            tracing::warn!(%object_id, %state, "unknown AI state");
            continue;
        };
        let Some(entity) = super::find_object(world, object_id) else {
            tracing::warn!(%object_id, "AI state target was not found");
            continue;
        };
        if world.get::<SceneAi>(entity).is_none() {
            tracing::warn!(%object_id, "object has no AI");
            continue;
        }
        let mut memory = world.get::<AiMemory>(entity).cloned().unwrap_or_default();
        memory.forced = Some(state);
        world.entity_mut(entity).insert(memory);
    }
}

/// The name of a Simple brain's current state, for `engine.get_ai_state`.
pub fn ai_state(world: &World, entity: Entity) -> Option<&'static str> {
    world
        .get::<AiMemory>(entity)
        .and_then(|memory| memory.state)
        .map(AiState::name)
}

/// The nodes `entity`'s behaviour tree visited on its last frame, in visiting
/// order, with what each answered. Empty for other brains.
pub fn behaviour_tree_trace(world: &World, entity: Entity) -> engine_scene::BtTrace {
    world
        .get::<AiMemory>(entity)
        .map(|memory| memory.tree.trace.clone())
        .unwrap_or_default()
}

/// `entity`'s world position.
pub(super) fn position(world: &World, entity: Entity) -> Option<Vec3> {
    world
        .get::<GlobalTransform>(entity)
        .map(|global| global.0.translation)
        .or_else(|| {
            world
                .get::<Transform>(entity)
                .map(|local| local.0.translation)
        })
}

/// `entity`'s world rotation.
pub(super) fn rotation(world: &World, entity: Entity) -> Quat {
    world
        .get::<GlobalTransform>(entity)
        .map(|global| global.0.rotation)
        .or_else(|| world.get::<Transform>(entity).map(|local| local.0.rotation))
        .unwrap_or(Quat::IDENTITY)
}

/// The direction an object faces, flattened onto the ground: its -Z, as
/// `engine.forward` reports.
pub(super) fn facing(world: &World, entity: Entity) -> Vec3 {
    let forward = rotation(world, entity) * Vec3::NEG_Z;
    Vec3::new(forward.x, 0.0, forward.z)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z)
}

/// `entity`'s stable scene ID.
pub(super) fn object_id(world: &World, entity: Entity) -> Option<String> {
    world.get::<SceneObjectId>(entity).map(|id| id.0.clone())
}

/// Every enabled object with AI, with its ID and settings.
pub(super) fn agents(world: &mut World) -> Vec<(Entity, String, engine_scene::AiData)> {
    world
        .query_filtered::<(Entity, &SceneObjectId, &SceneAi), engine_ecs::prelude::Without<engine_ecs::components::Disabled>>()
        .iter(world)
        .map(|(entity, id, ai)| (entity, id.0.clone(), ai.0.clone()))
        .collect()
}

/// A rotation that turns `current` toward facing `direction` (flattened onto
/// the ground) by at most `max_degrees`.
pub(super) fn turn_toward(current: Quat, direction: Vec3, max_degrees: f32) -> Option<Quat> {
    let flat = Vec3::new(direction.x, 0.0, direction.z).try_normalize()?;
    let goal = engine_scene::look_rotation(flat, Vec3::Y)?;
    let angle = current.angle_between(goal);
    if angle < 1e-4 {
        return None;
    }
    let step = max_degrees.to_radians();
    Some(if angle <= step {
        goal
    } else {
        current.slerp(goal, step / angle)
    })
}
