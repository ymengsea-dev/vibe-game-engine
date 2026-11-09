//! Applies [`GameCommand`]s to the ECS world.
//!
//! Pure world logic: no GPU, physics or UI access, so every rule here is
//! unit-testable. Prefab spawning needs asset resolution, so the caller
//! passes it in as a closure.

use std::collections::HashSet;

use engine_ecs::components::{Name, RigidBody, Transform as TransformComponent};
use engine_ecs::prelude::{ChildOf, Entity, World};
use engine_physics::{PhysicsWorld, RigidBodyHandle};
use engine_scene::{RuleVariables, Scene, SceneObjectId, SceneSpriteAnimation};
use engine_scripting::GameCommand;
use engine_utils::Transform;
use glam::{Quat, Vec3};

use super::GameplayError;

/// The live entity carrying a stable scene object ID, if any.
pub fn find_object(world: &World, object_id: &str) -> Option<Entity> {
    world
        .iter_entities()
        .find(|entity| {
            entity
                .get::<SceneObjectId>()
                .is_some_and(|id| id.0 == object_id)
        })
        .map(|entity| entity.id())
}

/// The stable IDs of every live object carrying `tag`.
pub fn find_objects_by_tag(world: &World, tag: &str) -> Vec<String> {
    world
        .iter_entities()
        .filter(|entity| {
            entity
                .get::<engine_scene::SceneTags>()
                .is_some_and(|tags| tags.0.iter().any(|candidate| candidate == tag))
        })
        .filter_map(|entity| entity.get::<SceneObjectId>().map(|id| id.0.clone()))
        .collect()
}

/// Spawns one prefab instance: `(world, path, object_id, position)`.
pub type PrefabSpawner<'a> =
    dyn FnMut(&mut World, &str, &str, [f32; 3]) -> Result<(Vec<Entity>, Scene), GameplayError> + 'a;

/// Applies the world-level commands in order and returns spawned prefab instances,
/// which still need colliders and sprite atlas updates from the caller.
///
/// UI text, mouse lock, body velocity/force/impulse, rule events, saves and scene changes are not world changes; they
/// are skipped here and handled by [`super::GameplayRuntime::apply`]. Invalid
/// requests and missing targets log a warning and do not stop the batch; a
/// failed prefab spawn does, because it means a broken package.
pub fn apply_world_commands(
    world: &mut World,
    commands: &[GameCommand],
    spawn_prefab: &mut PrefabSpawner<'_>,
) -> Result<Vec<(Vec<Entity>, Scene)>, GameplayError> {
    let mut spawned_prefabs = Vec::new();
    for command in commands {
        match command {
            GameCommand::Log(message) => tracing::info!("{message}"),
            GameCommand::RequestSave
            | GameCommand::ChangeScene { .. }
            | GameCommand::LoadScene { .. }
            | GameCommand::UnloadScene { .. }
            | GameCommand::NextScene
            | GameCommand::UiSetText { .. }
            | GameCommand::SetMouseLocked { .. }
            | GameCommand::EmitEvent { .. }
            | GameCommand::SetVelocity { .. }
            | GameCommand::AddForce { .. }
            | GameCommand::AddImpulse { .. }
            | GameCommand::MoveCharacter { .. }
            | GameCommand::SetCamera { .. }
            | GameCommand::Destroy { .. }
            | GameCommand::SetVisible { .. }
            | GameCommand::SetEnabled { .. }
            | GameCommand::SetFollowTarget { .. }
            | GameCommand::CameraShake { .. }
            | GameCommand::SetGameState { .. } => {}
            GameCommand::SetVariable { name, value } => {
                let mut variables = world.get_resource_or_insert_with(RuleVariables::default);
                if let Err(error) = variables.set(name, *value) {
                    tracing::warn!(variable = %name, %error, "gameplay variable was not changed");
                }
            }
            GameCommand::SetAnimationPlaying { object_id, playing } => {
                match find_object(world, object_id)
                    .and_then(|entity| world.get_mut::<SceneSpriteAnimation>(entity))
                {
                    Some(mut animation) => animation.playing = *playing,
                    None => tracing::warn!(%object_id, "animation target is missing an animation"),
                }
            }
            GameCommand::PlayAudio { object_id } => {
                match find_object(world, object_id)
                    .and_then(|entity| world.get_mut::<engine_ecs::AudioEmitter>(entity))
                {
                    Some(mut emitter) => emitter.play(),
                    None => tracing::warn!(%object_id, "audio target is missing an emitter"),
                }
            }
            GameCommand::SpawnEmpty {
                object_id,
                name,
                position,
            } => spawn_empty(world, object_id, name, *position),
            GameCommand::SpawnPrefab {
                object_id,
                path,
                position,
            } => spawned_prefabs.push(spawn_prefab(world, path, object_id, *position)?),
            GameCommand::SetPosition {
                object_id,
                position,
            } => move_object(world, object_id, *position, false),
            GameCommand::Translate { object_id, delta } => {
                move_object(world, object_id, *delta, true);
            }
            GameCommand::SetRotation {
                object_id,
                rotation,
            } => rotate_object(world, object_id, *rotation, false),
            GameCommand::Rotate { object_id, delta } => {
                rotate_object(world, object_id, *delta, true);
            }
            GameCommand::SetScale { object_id, scale } => scale_object(world, object_id, *scale),
        }
    }
    Ok(spawned_prefabs)
}

/// Whether a runtime object request has a fresh UUID and a finite position.
pub(super) fn valid_new_object(world: &World, object_id: &str, position: [f32; 3]) -> bool {
    uuid::Uuid::parse_str(object_id).is_ok()
        && position.iter().all(|value| value.is_finite())
        && find_object(world, object_id).is_none()
}

fn spawn_empty(world: &mut World, object_id: &str, name: &str, position: [f32; 3]) {
    if name.trim().is_empty() || name.len() > 256 || !valid_new_object(world, object_id, position) {
        tracing::warn!(%object_id, "spawn request was rejected");
        return;
    }
    world.spawn((
        SceneObjectId(object_id.to_owned()),
        Name::new(name),
        TransformComponent::from(Transform::from_translation(Vec3::from_array(position))),
    ));
}

fn move_object(world: &mut World, object_id: &str, value: [f32; 3], additive: bool) {
    if !value.iter().all(|component| component.is_finite()) {
        tracing::warn!(%object_id, "requested a non-finite position");
        return;
    }
    let Some(target) = find_object(world, object_id) else {
        tracing::warn!(%object_id, "move target object was not found");
        return;
    };
    let Some(mut transform) = world.get_mut::<TransformComponent>(target) else {
        tracing::warn!(%object_id, "move target has no transform");
        return;
    };
    let requested = Vec3::from_array(value);
    let next = if additive {
        transform.0.translation + requested
    } else {
        requested
    };
    if next.is_finite() {
        transform.0.translation = next;
    } else {
        tracing::warn!(%object_id, "movement exceeded the finite position range");
    }
}

fn rotate_object(world: &mut World, object_id: &str, value: [f32; 4], additive: bool) {
    let requested = Quat::from_array(value);
    if !requested.is_finite() || requested.length_squared() < 1e-12 {
        tracing::warn!(%object_id, "requested a non-finite or zero rotation");
        return;
    }
    let requested = requested.normalize();
    let Some(mut transform) = find_object(world, object_id)
        .and_then(|target| world.get_mut::<TransformComponent>(target))
    else {
        tracing::warn!(%object_id, "rotation target has no transform");
        return;
    };
    let next = if additive {
        transform.0.rotation * requested
    } else {
        requested
    };
    if next.is_finite() {
        transform.0.rotation = next.normalize();
    } else {
        tracing::warn!(%object_id, "rotation became non-finite");
    }
}

fn scale_object(world: &mut World, object_id: &str, value: [f32; 3]) {
    let scale = Vec3::from_array(value);
    if !scale.is_finite() || value.contains(&0.0) {
        tracing::warn!(%object_id, "requested a non-finite or zero scale");
        return;
    }
    let Some(mut transform) = find_object(world, object_id)
        .and_then(|target| world.get_mut::<TransformComponent>(target))
    else {
        tracing::warn!(%object_id, "scale target has no transform");
        return;
    };
    transform.0.scale = scale;
}

/// Object IDs moved or rotated by a command batch.
pub(super) fn moved_object_ids(commands: &[GameCommand]) -> Vec<&str> {
    commands
        .iter()
        .filter_map(|command| match command {
            GameCommand::SetPosition { object_id, .. }
            | GameCommand::Translate { object_id, .. }
            | GameCommand::SetRotation { object_id, .. }
            | GameCommand::Rotate { object_id, .. } => Some(object_id.as_str()),
            _ => None,
        })
        .collect()
}

/// Rigid bodies of moved, unparented objects with their new positions.
///
/// A parented body would need its parent's world pose; that is unsupported and
/// reported rather than silently teleported to the wrong place.
pub(super) fn moved_body_poses(
    world: &World,
    object_ids: &[&str],
) -> Vec<(RigidBodyHandle, Vec3, Quat)> {
    let moved: HashSet<_> = object_ids.iter().copied().collect();
    world
        .iter_entities()
        .filter_map(|entity| {
            let id = entity.get::<SceneObjectId>()?;
            if !moved.contains(id.0.as_str()) {
                return None;
            }
            // A parented object with no body, like a first-person camera, is fine.
            let body = entity.get::<RigidBody>()?.0;
            if entity.get::<ChildOf>().is_some() {
                tracing::warn!(object_id = %id.0, "moving a parented physics body is unsupported");
                return None;
            }
            Some((
                body,
                entity.get::<TransformComponent>()?.0.translation,
                entity.get::<TransformComponent>()?.0.rotation,
            ))
        })
        .collect()
}

/// Teleports bodies so physics starts the next step from the commanded pose.
pub(super) fn sync_moved_bodies(
    physics: &mut PhysicsWorld,
    poses: &[(RigidBodyHandle, Vec3, Quat)],
) {
    for (handle, position, rotation) in poses {
        if let Some(body) = physics.rapier.bodies.get_mut(*handle) {
            body.set_translation(*position, true);
            body.set_rotation(*rotation, true);
        }
    }
}
