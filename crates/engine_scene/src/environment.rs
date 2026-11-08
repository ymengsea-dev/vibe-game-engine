//! Scene-wide environment effects at runtime: ambient sounds.

use bevy_ecs::component::Component;
use engine_ecs::audio::AudioEmitter;
use engine_ecs::components::{GlobalTransform, Transform};
use engine_ecs::prelude::World;

use crate::{EnvironmentData, SceneResolver};

/// Marks a runtime entity that plays one of the scene's ambient sounds.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmbientSound;

/// Starts every ambient sound in `environment`, looping and non-positional,
/// on the audio bus at `bus` (from `GameContext::add_bus`). Returns how many
/// started; an unresolvable sound is logged and skipped so the scene still runs.
pub fn spawn_ambient_sounds(
    environment: &EnvironmentData,
    world: &mut World,
    resolver: &mut impl SceneResolver,
    bus: usize,
) -> usize {
    let mut started = 0;
    for ambient in &environment.ambient_sounds {
        match resolver.resolve_sound(&ambient.sound, true) {
            Ok(Some(sound)) => {
                let mut emitter = AudioEmitter::new(sound).on_bus(bus);
                emitter.gain = ambient.gain;
                world.spawn((
                    AmbientSound,
                    emitter,
                    Transform::default(),
                    GlobalTransform::default(),
                ));
                started += 1;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                sound = %ambient.sound.id,
                "skipped unresolvable ambient sound"
            ),
        }
    }
    started
}

#[cfg(test)]
mod tests {
    use crate::{AmbientSoundData, AssetRef, EnvironmentData, NullResolver, Scene};

    fn ambient(gain: f32) -> AmbientSoundData {
        AmbientSoundData {
            sound: AssetRef {
                id: uuid::Uuid::new_v4().to_string(),
            },
            gain,
        }
    }

    #[test]
    fn environment_round_trips_and_older_scenes_have_none() {
        let mut scene = Scene::default();
        assert!(!scene.to_ron_string().unwrap().contains("environment"));
        scene.environment.ambient_sounds.push(ambient(0.5));
        scene.validate().expect("valid environment");
        let reopened = Scene::from_ron_str(&scene.to_ron_string().unwrap()).unwrap();
        assert_eq!(reopened.environment, scene.environment);
        let old = Scene::from_ron_str("(version: 5, entities: [])").unwrap();
        assert!(old.environment.is_empty());
    }

    #[test]
    fn environment_rejects_bad_gain_ids_and_too_many_sounds() {
        let mut scene = Scene::default();
        scene.environment.ambient_sounds.push(ambient(f32::NAN));
        assert!(scene.validate().is_err());
        scene.environment.ambient_sounds[0] = ambient(5.0);
        assert!(scene.validate().is_err());
        scene.environment.ambient_sounds[0].gain = 1.0;
        scene.environment.ambient_sounds[0].sound.id = "not-a-uuid".into();
        assert!(scene.validate().is_err());
        scene.environment = EnvironmentData {
            ambient_sounds: (0..=EnvironmentData::MAX_AMBIENT_SOUNDS)
                .map(|_| ambient(1.0))
                .collect(),
            start_state: None,
        };
        assert!(scene.validate().is_err());
    }

    #[test]
    fn the_editor_world_carries_the_environment_so_undo_and_save_keep_it() {
        let mut scene = Scene::default();
        scene.environment.ambient_sounds.push(ambient(0.7));
        let mut world = engine_ecs::prelude::World::new();
        scene.instantiate_with_resolver(&mut world, &mut NullResolver);
        assert_eq!(Scene::from_world(&mut world).environment, scene.environment);
        assert_eq!(
            super::spawn_ambient_sounds(&scene.environment, &mut world, &mut NullResolver, 0),
            0,
            "nothing plays without a resolved sound"
        );
    }
}
