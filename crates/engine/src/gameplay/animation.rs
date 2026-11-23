//! Animation reaching gameplay: the named moments (a footstep, a sword
//! hit) animated models passed in their clips, and animators choosing
//! which clip a model plays.

use std::sync::Arc;

use engine_ecs::prelude::{Entity, World};
use engine_ecs::{AnimationClips, AnimationEvents, AnimationPlayer};
use engine_scene::{
    AnimatorData, AnimatorMotion, AnimatorState, AnimatorValues, RuleVariables,
    SceneModelAnimation, SceneObjectId,
};

/// Runs every animated model's animator once: starts it in its first
/// state, takes at most one transition, and keeps blend states mixed by
/// their number. Call once per frame while the game is not paused.
pub(super) fn update_animators(world: &mut World) {
    let animated: Vec<Entity> = world
        .query::<(Entity, &SceneModelAnimation, &AnimationPlayer)>()
        .iter(world)
        .filter(|(_, settings, _)| settings.0.animator.is_some())
        .map(|(entity, ..)| entity)
        .collect();
    for entity in animated {
        update_animator(world, entity);
    }
}

fn update_animator(world: &mut World, entity: Entity) {
    let state = match world.get::<AnimatorState>(entity) {
        Some(state) => state.clone(),
        None => {
            let Some(data) = world
                .get::<SceneModelAnimation>(entity)
                .and_then(|settings| settings.0.animator.clone())
                .filter(|data| !data.states.is_empty())
            else {
                return;
            };
            let state = AnimatorState {
                data: Arc::new(data),
                current: 0,
            };
            enter_state(world, entity, &state.data, 0, 0.0);
            world.entity_mut(entity).insert(state.clone());
            state
        }
    };
    let value = |name: &str| animator_value(world, entity, name);
    let ended = world
        .get::<AnimationPlayer>(entity)
        .is_some_and(|player| player.ended);
    let mut current = state.current;
    if let Some((target, fade)) = state.data.next_transition(current, value, ended) {
        enter_state(world, entity, &state.data, target, fade);
        current = target;
        if let Some(mut live) = world.get_mut::<AnimatorState>(entity) {
            live.current = target;
        }
    }
    mix_blend(world, entity, &state.data, current);
}

/// The number `name` for `entity`: its own animator value, else the game's
/// shared variable, else 0.
fn animator_value(world: &World, entity: Entity, name: &str) -> f64 {
    world
        .get::<AnimatorValues>(entity)
        .and_then(|values| values.0.get(name).copied())
        .or_else(|| {
            world
                .get_resource::<RuleVariables>()
                .map(|variables| variables.get(name))
        })
        .unwrap_or(0.0)
}

fn enter_state(world: &mut World, entity: Entity, data: &AnimatorData, index: usize, fade: f32) {
    let Some(state) = data.states.get(index) else {
        return;
    };
    let first = match &state.motion {
        AnimatorMotion::Clip(clip) => clip.as_str(),
        AnimatorMotion::Blend { clips, .. } => match clips.first() {
            Some(first) => first.clip.as_str(),
            None => return,
        },
    };
    let Some(clip) = world
        .get::<AnimationClips>(entity)
        .and_then(|clips| clips.get(first))
        .cloned()
    else {
        tracing::warn!(state = %state.name, clip = first, "animator state names a clip the model does not have");
        return;
    };
    if let Some(mut player) = world.get_mut::<AnimationPlayer>(entity) {
        player.play(clip, state.looping, state.speed, fade);
    }
    mix_blend(world, entity, data, index);
}

/// Keeps a blend state's mix following its number; nothing for a clip state.
fn mix_blend(world: &mut World, entity: Entity, data: &AnimatorData, index: usize) {
    let Some(AnimatorMotion::Blend { variable, clips }) =
        data.states.get(index).map(|state| &state.motion)
    else {
        return;
    };
    let parameter = animator_value(world, entity, variable) as f32;
    let Some(available) = world.get::<AnimationClips>(entity) else {
        return;
    };
    let mixed: Vec<_> = clips
        .iter()
        .filter_map(|blend| {
            available
                .get(&blend.clip)
                .map(|clip| (blend.at, Arc::clone(clip)))
        })
        .collect();
    if let Some(mut player) = world.get_mut::<AnimationPlayer>(entity) {
        player.set_blend(mixed, parameter);
    }
}

/// One clip event an object's animation passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationEventFired {
    /// Stable scene object ID of the animated object.
    pub object_id: String,
    /// The event's name in the clip.
    pub name: String,
}

/// Takes the events every animated object passed since the last call, in
/// world order. Taking (not reading) them means a frame where the game
/// does not step cannot deliver the same footstep twice.
pub(super) fn take_animation_events(world: &mut World) -> Vec<AnimationEventFired> {
    let mut fired = Vec::new();
    let mut query = world.query::<(&SceneObjectId, &mut AnimationEvents)>();
    for (id, mut events) in query.iter_mut(world) {
        if events.0.is_empty() {
            continue;
        }
        fired.extend(
            std::mem::take(&mut events.0)
                .into_iter()
                .map(|name| AnimationEventFired {
                    object_id: id.0.clone(),
                    name,
                }),
        );
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_taken_once_with_their_object() {
        let mut world = World::new();
        world.spawn((
            SceneObjectId("hero".into()),
            AnimationEvents(vec!["step".into(), "step_right".into()]),
        ));
        world.spawn(AnimationEvents(vec!["no_id".into()]));

        let fired = take_animation_events(&mut world);
        assert_eq!(
            fired,
            vec![
                AnimationEventFired {
                    object_id: "hero".into(),
                    name: "step".into()
                },
                AnimationEventFired {
                    object_id: "hero".into(),
                    name: "step_right".into()
                },
            ]
        );
        assert!(take_animation_events(&mut world).is_empty(), "taken");
    }
}
