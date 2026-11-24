//! Senses: what each object with AI sees and hears this frame.

use engine_ecs::components::Collider;
use engine_ecs::prelude::{Entity, World};
use engine_physics::{PhysicsWorld, QueryFilter, cast_ray_filtered};
use engine_scene::{SceneObjectId, SceneTags};
use glam::Vec3;

use super::{AiEvent, AiMemory, AiTick};

/// A noise made this frame, from `engine.make_noise` or a rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::gameplay) struct Noise {
    /// Where it was made.
    pub position: Vec3,
    /// How far it carries.
    pub radius: f32,
}

/// How far short of a target a sight ray may stop and still count as
/// reaching it (the ray aims at the target's position, inside its collider).
const SIGHT_SLACK: f32 = 0.25;

/// Updates every object's [`AiMemory`] from what it sees and hears, and
/// reports targets noticed and lost and noises heard.
pub(super) fn update(
    world: &mut World,
    physics: &PhysicsWorld,
    noises: &[Noise],
    dt: f32,
    out: &mut AiTick,
) {
    for (entity, object_id, ai) in super::agents(world) {
        let Some(senses) = ai.senses else {
            continue;
        };
        let Some(position) = super::position(world, entity) else {
            continue;
        };
        let mut memory = world.get::<AiMemory>(entity).cloned().unwrap_or_default();
        let eye = position + Vec3::Y * senses.eye_height;
        let forward = super::facing(world, entity);

        let mut found: Option<(f32, String, Vec3, bool)> = None;
        for (target, target_id, target_position) in targets(world, entity, &senses.target_tag) {
            let distance = target_position.distance(position);
            let sees = distance <= senses.sight_range
                && in_view(forward, target_position - position, senses.sight_angle)
                && clear_view(world, physics, entity, target, eye, target_position);
            let hears = distance <= senses.hearing_radius;
            if !(sees || hears) {
                continue;
            }
            // Prefer the target it already has, then the closest one seen.
            let rank = if memory.target.as_deref() == Some(target_id.as_str()) {
                -1.0
            } else if sees {
                distance
            } else {
                distance + senses.sight_range.max(senses.hearing_radius)
            };
            if found.as_ref().is_none_or(|(best, ..)| rank < *best) {
                found = Some((rank, target_id, target_position, sees));
            }
        }

        match found {
            Some((_, target_id, target_position, sees)) => {
                if memory.target.as_deref() != Some(target_id.as_str()) {
                    if let Some(previous) = memory.target.take() {
                        out.events.push(AiEvent::TargetLost {
                            object_id: object_id.clone(),
                            target_id: previous,
                        });
                    }
                    out.events.push(AiEvent::TargetSeen {
                        object_id: object_id.clone(),
                        target_id: target_id.clone(),
                    });
                }
                memory.target = Some(target_id);
                memory.sees = sees;
                memory.hears = target_position.distance(position) <= senses.hearing_radius;
                memory.since_contact = 0.0;
                memory.last_known = Some(target_position);
            }
            None => {
                memory.sees = false;
                memory.hears = false;
                if memory.target.is_some() {
                    memory.since_contact += dt;
                    if memory.since_contact >= senses.forget_after
                        && let Some(previous) = memory.target.take()
                    {
                        out.events.push(AiEvent::TargetLost {
                            object_id: object_id.clone(),
                            target_id: previous,
                        });
                    }
                }
            }
        }

        if let Some(noise) = noises
            .iter()
            .filter(|noise| noise.position.distance(position) <= noise.radius)
            .min_by(|a, b| {
                a.position
                    .distance(position)
                    .total_cmp(&b.position.distance(position))
            })
        {
            memory.noise = Some(noise.position);
            out.events.push(AiEvent::NoiseHeard {
                object_id: object_id.clone(),
                position: noise.position.to_array(),
            });
        }
        world.entity_mut(entity).insert(memory);
    }
}

/// Every enabled object tagged `tag` other than `seeker`, with its ID and
/// position.
fn targets(world: &mut World, seeker: Entity, tag: &str) -> Vec<(Entity, String, Vec3)> {
    let tagged: Vec<(Entity, String)> = world
        .query_filtered::<(Entity, &SceneObjectId, &SceneTags), engine_ecs::prelude::Without<engine_ecs::components::Disabled>>()
        .iter(world)
        .filter(|(entity, _, tags)| *entity != seeker && tags.0.iter().any(|t| t == tag))
        .map(|(entity, id, _)| (entity, id.0.clone()))
        .collect();
    tagged
        .into_iter()
        .filter_map(|(entity, id)| Some((entity, id, super::position(world, entity)?)))
        .collect()
}

/// Whether `offset` lies inside a view `angle` degrees wide around `forward`,
/// measured on the ground.
fn in_view(forward: Vec3, offset: Vec3, angle: f32) -> bool {
    if angle >= 360.0 {
        return true;
    }
    let Some(flat) = Vec3::new(offset.x, 0.0, offset.z).try_normalize() else {
        return true;
    };
    flat.dot(forward) >= (angle * 0.5).to_radians().cos()
}

/// Whether nothing solid stands between `eye` and the target. Triggers and
/// the seeker's own collider never block.
fn clear_view(
    world: &World,
    physics: &PhysicsWorld,
    seeker: Entity,
    target: Entity,
    eye: Vec3,
    target_position: Vec3,
) -> bool {
    line_of_sight(
        world,
        physics,
        Some(seeker),
        Some(target),
        eye,
        target_position,
    )
}

/// Whether a straight line from `from` to `to` is clear of solid colliders,
/// other than `ignore`'s and `target`'s.
pub(in crate::gameplay) fn line_of_sight(
    world: &World,
    physics: &PhysicsWorld,
    ignore: Option<Entity>,
    target: Option<Entity>,
    from: Vec3,
    to: Vec3,
) -> bool {
    let offset = to - from;
    let distance = offset.length();
    if distance < 1e-4 {
        return true;
    }
    let mut filter = QueryFilter::default().exclude_sensors();
    if let Some(handle) = ignore.and_then(|entity| world.get::<Collider>(entity)) {
        filter = filter.exclude_collider(handle.0);
    }
    let target_collider = target.and_then(|entity| world.get::<Collider>(entity).map(|c| c.0));
    match cast_ray_filtered(physics, from, offset / distance, distance, filter) {
        Ok(Some(hit)) => {
            Some(hit.collider) == target_collider || hit.distance >= distance - SIGHT_SLACK
        }
        Ok(None) | Err(_) => true,
    }
}
