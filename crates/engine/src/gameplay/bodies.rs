//! Velocity, force and impulse commands applied to physics bodies.

use std::collections::{HashMap, HashSet};

use engine_ecs::components::{Collider, RigidBody};
use engine_ecs::prelude::{ChildOf, World};
use engine_physics::{
    CharacterAutostep, CharacterController, CharacterLength, PhysicsWorld, QueryFilter,
    cast_ray_filtered, move_character,
};
use engine_scene::SceneObjectId;
use engine_scripting::{GameCommand, ScriptQueries, ScriptRayHit};
use glam::Vec3;

use super::commands::find_object;

/// Applies every body command in the batch, in order.
pub(super) fn apply_body_commands(
    world: &World,
    physics: &mut PhysicsWorld,
    warned: &mut HashSet<(String, &'static str)>,
    grounded: &mut HashMap<String, bool>,
    commands: &[GameCommand],
    fixed_step: f32,
) {
    for command in commands {
        apply_body_command(world, physics, warned, grounded, command, fixed_step);
    }
}

/// Applies one body command; other commands are ignored.
///
/// A missing object, a missing body or a non-dynamic body skips the command
/// and warns once per `(object_id, kind)` until `warned` is cleared.
/// `AddForce` becomes the impulse `force * duration`.
pub(super) fn apply_body_command(
    world: &World,
    physics: &mut PhysicsWorld,
    warned: &mut HashSet<(String, &'static str)>,
    grounded: &mut HashMap<String, bool>,
    command: &GameCommand,
    fixed_step: f32,
) {
    if let GameCommand::MoveCharacter { object_id, delta } = command {
        move_character_command(
            world,
            physics,
            warned,
            grounded,
            object_id,
            Vec3::from_array(*delta),
            fixed_step,
        );
        return;
    }
    let (object_id, kind, value) = match command {
        GameCommand::SetVelocity {
            object_id,
            velocity,
        } => (object_id, "set_velocity", Vec3::from_array(*velocity)),
        GameCommand::AddForce {
            object_id,
            force,
            duration,
        } => (
            object_id,
            "add_force",
            if duration.is_finite() && *duration >= 0.0 {
                Vec3::from_array(*force) * *duration
            } else {
                Vec3::NAN
            },
        ),
        GameCommand::AddImpulse { object_id, impulse } => {
            (object_id, "add_impulse", Vec3::from_array(*impulse))
        }
        _ => return,
    };
    if !value.is_finite() {
        warn_once(warned, object_id, kind, "value is not finite");
        return;
    }
    let Some(entity) = find_object(world, object_id) else {
        warn_once(warned, object_id, kind, "object was not found");
        return;
    };
    let Some(handle) = world.get::<RigidBody>(entity).map(|body| body.0) else {
        warn_once(warned, object_id, kind, "object has no physics body");
        return;
    };
    let Some(body) = physics.rapier.bodies.get_mut(handle) else {
        warn_once(warned, object_id, kind, "physics body is gone");
        return;
    };
    if !body.is_dynamic() {
        warn_once(warned, object_id, kind, "body is not dynamic");
        return;
    }
    if kind == "set_velocity" {
        body.set_linvel(value, true);
    } else {
        body.apply_impulse(value, true);
    }
}

/// Moves a kinematic character with collision response and remembers whether
/// it ended up grounded. Non-kinematic, missing or parented targets are
/// skipped with a one-time warning.
fn move_character_command(
    world: &World,
    physics: &mut PhysicsWorld,
    warned: &mut HashSet<(String, &'static str)>,
    grounded: &mut HashMap<String, bool>,
    object_id: &str,
    delta: Vec3,
    fixed_step: f32,
) {
    const KIND: &str = "move_character";
    if !delta.is_finite() {
        warn_once(warned, object_id, KIND, "value is not finite");
        return;
    }
    let Some(entity) = find_object(world, object_id) else {
        warn_once(warned, object_id, KIND, "object was not found");
        return;
    };
    if world.get::<ChildOf>(entity).is_some() {
        warn_once(
            warned,
            object_id,
            KIND,
            "parented characters are unsupported",
        );
        return;
    }
    let (Some(body), Some(collider)) = (
        world.get::<RigidBody>(entity).map(|body| body.0),
        world.get::<Collider>(entity).map(|collider| collider.0),
    ) else {
        warn_once(warned, object_id, KIND, "object has no physics body");
        return;
    };
    let Some(is_kinematic) = physics
        .rapier
        .bodies
        .get(body)
        .map(|body| body.is_kinematic())
    else {
        warn_once(warned, object_id, KIND, "physics body is gone");
        return;
    };
    if !is_kinematic {
        warn_once(warned, object_id, KIND, "body is not kinematic");
        return;
    }
    let controller = CharacterController {
        autostep: Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(0.3),
            min_width: CharacterLength::Absolute(0.2),
            include_dynamic_bodies: false,
        }),
        snap_to_ground: Some(CharacterLength::Absolute(0.2)),
        ..CharacterController::default()
    };
    match move_character(physics, &controller, body, collider, delta, fixed_step) {
        Ok(movement) => {
            grounded.insert(object_id.to_owned(), movement.grounded);
        }
        Err(error) => {
            let reason = error.to_string();
            warn_once(warned, object_id, KIND, &reason);
        }
    }
}

/// Longest downward probe that counts as "standing on something".
const GROUND_PROBE: f32 = 0.06;
/// How far above the collider's bottom the probe starts.
const GROUND_PROBE_LIFT: f32 = 0.01;

/// World queries for scripts and Rust games, valid for one callback.
pub struct GameplayQueries<'a> {
    pub(super) world: &'a World,
    pub(super) physics: &'a PhysicsWorld,
    pub(super) grounded: Option<&'a HashMap<String, bool>>,
}

impl<'a> GameplayQueries<'a> {
    /// Queries over `world` and `physics` with no remembered `move_character` results,
    /// so `is_grounded` always probes with a short ray.
    pub fn new(world: &'a World, physics: &'a PhysicsWorld) -> Self {
        Self {
            world,
            physics,
            grounded: None,
        }
    }
}

impl ScriptQueries for GameplayQueries<'_> {
    fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        ignore: Option<&str>,
    ) -> Option<ScriptRayHit> {
        let mut filter = QueryFilter::default().exclude_sensors();
        if let Some(handle) = ignore
            .and_then(|id| find_object(self.world, id))
            .and_then(|entity| self.world.get::<Collider>(entity).map(|c| c.0))
        {
            filter = filter.exclude_collider(handle);
        }
        let hit = cast_ray_filtered(
            self.physics,
            Vec3::from_array(origin),
            Vec3::from_array(direction),
            max_distance,
            filter,
        )
        .ok()??;
        Some(ScriptRayHit {
            object_id: self.object_of(hit.collider),
            point: hit.point.to_array(),
            normal: hit.normal.to_array(),
            distance: hit.distance,
        })
    }

    fn is_grounded(&self, object_id: &str) -> bool {
        if let Some(value) = self.grounded.and_then(|map| map.get(object_id)) {
            return *value;
        }
        let Some(entity) = find_object(self.world, object_id) else {
            return false;
        };
        let Some(handle) = self.world.get::<Collider>(entity).map(|c| c.0) else {
            return false;
        };
        let Some(collider) = self.physics.rapier.colliders.get(handle) else {
            return false;
        };
        let aabb = collider.compute_aabb();
        let center = aabb.center();
        let start = Vec3::new(center.x, aabb.mins.y + GROUND_PROBE_LIFT, center.z);
        let filter = QueryFilter::default()
            .exclude_sensors()
            .exclude_collider(handle);
        matches!(
            cast_ray_filtered(self.physics, start, Vec3::NEG_Y, GROUND_PROBE, filter),
            Ok(Some(_))
        )
    }
}

impl GameplayQueries<'_> {
    /// The scene object that owns `collider`.
    fn object_of(&self, collider: engine_physics::ColliderHandle) -> Option<String> {
        self.world.iter_entities().find_map(|entity| {
            (entity.get::<Collider>()?.0 == collider)
                .then(|| entity.get::<SceneObjectId>().map(|id| id.0.clone()))
                .flatten()
        })
    }
}

fn warn_once(
    warned: &mut HashSet<(String, &'static str)>,
    object_id: &str,
    kind: &'static str,
    reason: &str,
) {
    if warned.insert((object_id.to_owned(), kind)) {
        tracing::warn!(%object_id, kind, reason, "physics command skipped");
    }
}
