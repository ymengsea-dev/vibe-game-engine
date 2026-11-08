//! Camera, lights and audio listener derived from the authored scene.

use engine_ecs::AudioListener;
use engine_ecs::components::{GlobalTransform, Transform as TransformComponent};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{Camera, LightSet};
use engine_scene::Scene;

/// The scene's lighting override (or `defaults`) plus every authored point light.
pub fn scene_lights(scene: &Scene, world: &mut World, defaults: &LightSet) -> LightSet {
    let mut lights = scene
        .lighting
        .map(|lighting| lighting.to_light_set())
        .unwrap_or_else(|| defaults.clone());
    lights
        .point
        .extend(engine_scene::point_lights_from_world(world));
    lights
}

/// Places the spatial-audio listener at the camera, facing where it looks.
pub fn spawn_audio_listener(world: &mut World, camera: &Camera) -> Entity {
    let transform = super::camera::listener_transform(camera);
    world
        .spawn((
            TransformComponent::from(transform),
            GlobalTransform::from(transform),
            AudioListener::default(),
        ))
        .id()
}
