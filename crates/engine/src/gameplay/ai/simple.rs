//! The Simple brain: idle or patrol, chase what it notices, attack when
//! close, and go back once the target is lost.

use engine_ecs::prelude::{Entity, World};
use engine_scene::{AiBrainData, AiState, SimpleBrainData};
use engine_scripting::GameCommand;
use glam::Vec3;

use super::nav::{self, flat};
use super::{AiEvent, AiMemory, AiTick};

/// How often a chase works out a new route to a moving target, in seconds.
const CHASE_REPATH: f32 = 0.5;
/// How far a target may move from the end of a chase route before the route
/// is worked out again at once.
const CHASE_DRIFT: f32 = 0.75;
/// Crossfade between state clips, in seconds.
const CLIP_FADE: f32 = 0.2;

/// Runs every Simple brain for one frame.
pub(super) fn update(world: &mut World, dt: f32, out: &mut AiTick) {
    for (entity, object_id, ai) in super::agents(world) {
        let Some(AiBrainData::Simple(brain)) = &ai.brain else {
            continue;
        };
        let Some(position) = super::position(world, entity) else {
            continue;
        };
        let mut memory = world.get::<AiMemory>(entity).cloned().unwrap_or_default();
        let target = memory.target.as_deref().and_then(|id| {
            let found = super::super::find_object(world, id)?;
            super::position(world, found)
        });
        let in_contact = memory.sees || memory.hears;
        let target_at = if in_contact {
            target.or(memory.last_known)
        } else {
            memory.last_known
        };

        let wanted = memory
            .forced
            .take()
            .unwrap_or_else(|| match (&memory.target, target_at) {
                (Some(_), Some(at))
                    if in_contact && flat(at).distance(flat(position)) <= brain.attack_range =>
                {
                    AiState::Attack
                }
                (Some(_), Some(_)) => AiState::Chase,
                _ => brain.start,
            });
        if memory.state != Some(wanted) {
            enter(world, entity, &object_id, brain, &mut memory, wanted, out);
        }

        match wanted {
            AiState::Idle => idle(world, entity, &object_id, brain, &mut memory, dt, out),
            AiState::Patrol => patrol(world, entity, &object_id, brain, &mut memory, dt, out),
            AiState::Chase => {
                if let Some(at) = target_at {
                    memory.repath -= dt;
                    let drifted =
                        nav::goal(world, entity).is_none_or(|goal| goal.distance(at) > CHASE_DRIFT);
                    if memory.repath <= 0.0 || drifted {
                        nav::start_route(world, entity, at, Some(brain.chase_speed));
                        memory.repath = CHASE_REPATH;
                    }
                } else {
                    nav::stop(world, entity);
                }
            }
            AiState::Attack => {
                nav::stop(world, entity);
                if let Some(at) = target_at
                    && ai.agent.turn_speed > 0.0
                    && let Some(turned) = super::turn_toward(
                        super::rotation(world, entity),
                        at - position,
                        ai.agent.turn_speed * dt,
                    )
                {
                    out.commands.push(GameCommand::SetRotation {
                        object_id: object_id.clone(),
                        rotation: turned.to_array(),
                    });
                }
                memory.cooldown -= dt;
                if memory.cooldown <= 0.0 {
                    memory.cooldown = brain.attack_cooldown;
                    if !brain.attack_event.is_empty() {
                        out.commands.push(GameCommand::EmitEvent {
                            name: brain.attack_event.clone(),
                        });
                    }
                    play(out, &object_id, &brain.clips.attack, false);
                }
            }
        }
        if memory.state != Some(AiState::Attack) {
            memory.cooldown = (memory.cooldown - dt).max(0.0);
        }
        world.entity_mut(entity).insert(memory);
    }
}

/// Moves the brain into `state`: resets what that state keeps, starts its
/// clip and reports the change.
fn enter(
    world: &mut World,
    entity: Entity,
    object_id: &str,
    brain: &SimpleBrainData,
    memory: &mut AiMemory,
    state: AiState,
    out: &mut AiTick,
) {
    memory.state = Some(state);
    match state {
        AiState::Idle | AiState::Patrol => {
            nav::stop(world, entity);
            memory.walking_patrol = false;
            memory.investigating = false;
            memory.wait = 0.0;
        }
        AiState::Chase => memory.repath = 0.0,
        AiState::Attack => {}
    }
    let clip = match state {
        // A patrol shows its walk once it sets off; an attack plays per hit.
        AiState::Patrol | AiState::Attack => None,
        other => brain.clips.for_state(other),
    };
    if let Some(clip) = clip {
        play(out, object_id, clip, true);
    }
    out.events.push(AiEvent::StateChanged {
        object_id: object_id.to_owned(),
        state: state.name(),
    });
}

/// Stands still, or walks to a noise it heard and looks around there.
fn idle(
    world: &mut World,
    entity: Entity,
    object_id: &str,
    brain: &SimpleBrainData,
    memory: &mut AiMemory,
    dt: f32,
    out: &mut AiTick,
) {
    investigate(world, entity, object_id, brain, memory, dt, out);
}

/// Walks the patrol points, waiting at each, after checking any noise.
fn patrol(
    world: &mut World,
    entity: Entity,
    object_id: &str,
    brain: &SimpleBrainData,
    memory: &mut AiMemory,
    dt: f32,
    out: &mut AiTick,
) {
    if investigate(world, entity, object_id, brain, memory, dt, out) {
        return;
    }
    let count = brain.patrol_points.len();
    if count == 0 {
        return;
    }
    if nav::is_walking(world, entity) {
        return;
    }
    if memory.walking_patrol {
        memory.walking_patrol = false;
        memory.wait = brain.patrol_wait;
        memory.patrol_index = next_patrol_index(memory, count, brain.patrol_back_and_forth);
        play(out, object_id, &brain.clips.idle, true);
        return;
    }
    if memory.wait > 0.0 {
        memory.wait -= dt;
        return;
    }
    let point = Vec3::from_array(brain.patrol_points[memory.patrol_index.min(count - 1)]);
    if nav::start_route(world, entity, point, None) {
        memory.walking_patrol = true;
        play(out, object_id, &brain.clips.walk, true);
    } else {
        // Unreachable: skip it rather than stand there forever.
        memory.patrol_index = next_patrol_index(memory, count, brain.patrol_back_and_forth);
        memory.wait = brain.patrol_wait;
    }
}

/// Handles a heard noise: walks to it, then looks around for the patrol
/// wait. Returns whether that took this frame.
fn investigate(
    world: &mut World,
    entity: Entity,
    object_id: &str,
    brain: &SimpleBrainData,
    memory: &mut AiMemory,
    dt: f32,
    out: &mut AiTick,
) -> bool {
    if let Some(noise) = memory.noise.take() {
        if nav::start_route(world, entity, noise, None) {
            memory.investigating = true;
            memory.walking_patrol = false;
            memory.wait = brain.patrol_wait;
            play(out, object_id, &brain.clips.walk, true);
        }
        return memory.investigating;
    }
    if !memory.investigating {
        return false;
    }
    if nav::is_walking(world, entity) {
        return true;
    }
    if memory.wait > 0.0 {
        memory.wait -= dt;
        return true;
    }
    memory.investigating = false;
    play(out, object_id, &brain.clips.idle, true);
    true
}

/// The patrol point after the current one.
fn next_patrol_index(memory: &mut AiMemory, count: usize, back_and_forth: bool) -> usize {
    let at = memory.patrol_index.min(count - 1);
    if !back_and_forth || count < 2 {
        return (at + 1) % count;
    }
    if memory.patrol_reverse {
        if at == 0 {
            memory.patrol_reverse = false;
            1
        } else {
            at - 1
        }
    } else if at + 1 >= count {
        memory.patrol_reverse = true;
        count - 2
    } else {
        at + 1
    }
}

/// Plays `clip` on the object, if one is set.
fn play(out: &mut AiTick, object_id: &str, clip: &str, looping: bool) {
    if clip.is_empty() {
        return;
    }
    out.commands.push(GameCommand::PlayAnimation {
        object_id: object_id.to_owned(),
        clip: clip.to_owned(),
        looping: Some(looping),
        speed: 1.0,
        fade: CLIP_FADE,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forth_patrols_turn_at_each_end() {
        let mut memory = AiMemory::default();
        let mut order = Vec::new();
        for _ in 0..6 {
            memory.patrol_index = next_patrol_index(&mut memory, 3, true);
            order.push(memory.patrol_index);
        }
        assert_eq!(order, [1, 2, 1, 0, 1, 2]);
        memory.patrol_index = 2;
        assert_eq!(next_patrol_index(&mut memory, 3, false), 0);
        assert_eq!(next_patrol_index(&mut AiMemory::default(), 1, true), 0);
    }
}
