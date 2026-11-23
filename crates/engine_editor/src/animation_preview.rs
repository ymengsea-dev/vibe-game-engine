//! Playing an animated model's clip in the Scene View.
//!
//! In Studio nothing animates by itself: a model stands in the first frame
//! of its clip. The Inspector's Preview button marks the model with
//! [`AnimationPreview`] and starts its [`AnimationPlayer`]; the viewport
//! then calls [`tick`] each frame to move the clock and upload the pose.
//! When the clip stops (Stop, or a one-shot reaching its end) the marker
//! is removed after one last pose upload. None of this is saved: only the
//! authored [`SceneModelAnimation`] settings are.

use engine_ecs::components::SkinnedMeshRenderer;
use engine_ecs::prelude::{Component, Entity, World};
use engine_ecs::{AnimationClips, AnimationPlayer};
use engine_renderer::GpuContext;
use engine_scene::{ModelAnimationData, SceneModelAnimation};

/// Studio-only marker: the viewport drives this model's player.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AnimationPreview;

/// The longest step one preview frame takes, so a stall in the editor
/// does not jump the clip ahead.
const MAX_STEP_SECONDS: f32 = 0.1;

/// Moves every previewed model on by `dt` seconds and uploads its pose.
/// Returns whether any preview is still playing (the caller keeps
/// redrawing while it is).
pub fn tick(world: &mut World, gpu: &GpuContext, dt: f32) -> bool {
    let step = dt.clamp(0.0, MAX_STEP_SECONDS);
    let mut finished = Vec::new();
    let mut playing = false;
    let mut query = world.query::<(
        Entity,
        &AnimationPreview,
        &mut AnimationPlayer,
        &SkinnedMeshRenderer,
    )>();
    for (entity, _, mut player, skinned) in query.iter_mut(world) {
        player.advance(step);
        engine_ecs::upload_pose(gpu, &player, skinned);
        if player.playing {
            playing = true;
        } else {
            finished.push(entity);
        }
    }
    for entity in finished {
        world.entity_mut(entity).remove::<AnimationPreview>();
    }
    playing
}

/// The model's authored settings, or the defaults when it has none yet.
pub fn settings(world: &World, entity: Entity) -> ModelAnimationData {
    world
        .get::<SceneModelAnimation>(entity)
        .map(|animation| animation.0.clone())
        .unwrap_or_default()
}

/// Stores new authored settings and shows them on the live model: its
/// player switches to the chosen clip, paused at the start unless a
/// preview is running. Returns whether anything changed.
pub fn set_settings(world: &mut World, entity: Entity, value: ModelAnimationData) -> bool {
    if settings(world, entity) == value && world.get::<SceneModelAnimation>(entity).is_some() {
        return false;
    }
    let previewing = is_playing(world, entity);
    let clip = world.get::<AnimationClips>(entity).and_then(|clips| {
        value
            .clip
            .as_deref()
            .and_then(|name| clips.get(name))
            .or_else(|| clips.0.first())
            .cloned()
    });
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return false;
    };
    if let (Some(clip), Some(mut player)) = (clip, entity_mut.get_mut::<AnimationPlayer>()) {
        player.play(clip, value.looping, value.speed, 0.0);
        player.playing = previewing;
    }
    entity_mut.insert((SceneModelAnimation(value), AnimationPreview));
    true
}

/// Whether the model's clip is playing in the Scene View now.
pub fn is_playing(world: &World, entity: Entity) -> bool {
    world.get::<AnimationPreview>(entity).is_some()
        && world
            .get::<AnimationPlayer>(entity)
            .is_some_and(|player| player.playing)
}

/// Starts or stops the Scene View preview. Stopping returns the model to
/// the clip's first frame.
pub fn set_playing(world: &mut World, entity: Entity, play: bool) {
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    let Some(mut player) = entity_mut.get_mut::<AnimationPlayer>() else {
        return;
    };
    // A one-shot that reached its end plays again from the start.
    if !play || player.time >= player.clip.duration {
        player.restart();
    }
    player.playing = play;
    entity_mut.insert(AnimationPreview);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;

    fn clip(name: &str, duration: f32) -> Arc<engine_asset::ImportedAnimation> {
        Arc::new(engine_asset::ImportedAnimation {
            name: Some(name.into()),
            events: Vec::new(),
            duration,
            channels: HashMap::new(),
        })
    }

    fn model(world: &mut World) -> Entity {
        let skeleton = Arc::new(engine_asset::ImportedSkeleton {
            name: None,
            joints: Vec::new(),
        });
        let idle = clip("idle", 1.0);
        let mut player = AnimationPlayer::new(skeleton, Arc::clone(&idle));
        player.playing = false;
        world
            .spawn((AnimationClips(vec![idle, clip("wave", 0.5)]), player))
            .id()
    }

    #[test]
    fn choosing_a_clip_saves_it_and_shows_it_paused() {
        let mut world = World::new();
        let entity = model(&mut world);
        let value = ModelAnimationData {
            clip: Some("wave".into()),
            looping: false,
            ..Default::default()
        };

        assert!(set_settings(&mut world, entity, value.clone()));
        assert!(
            !set_settings(&mut world, entity, value.clone()),
            "no change"
        );

        assert_eq!(settings(&world, entity), value);
        let player = world.get::<AnimationPlayer>(entity).expect("player");
        assert_eq!(player.clip.name.as_deref(), Some("wave"));
        assert!(!player.looping && !player.playing);
        assert!(
            world.get::<AnimationPreview>(entity).is_some(),
            "pose refresh"
        );
    }

    #[test]
    fn preview_plays_stops_and_restarts_an_ended_one_shot() {
        let mut world = World::new();
        let entity = model(&mut world);
        set_playing(&mut world, entity, true);
        assert!(is_playing(&world, entity));

        if let Some(mut player) = world.get_mut::<AnimationPlayer>(entity) {
            player.time = 0.6;
        }
        set_playing(&mut world, entity, false);
        assert!(!is_playing(&world, entity));
        assert_eq!(
            world.get::<AnimationPlayer>(entity).map(|p| p.time),
            Some(0.0)
        );

        if let Some(mut player) = world.get_mut::<AnimationPlayer>(entity) {
            player.time = player.clip.duration;
        }
        set_playing(&mut world, entity, true);
        assert_eq!(
            world.get::<AnimationPlayer>(entity).map(|p| p.time),
            Some(0.0)
        );
    }

    #[test]
    fn changing_settings_while_previewing_keeps_playing() {
        let mut world = World::new();
        let entity = model(&mut world);
        set_playing(&mut world, entity, true);
        set_settings(
            &mut world,
            entity,
            ModelAnimationData {
                speed: 2.0,
                ..Default::default()
            },
        );
        assert!(is_playing(&world, entity));
        assert_eq!(
            world.get::<AnimationPlayer>(entity).map(|p| p.speed),
            Some(2.0)
        );
    }
}
