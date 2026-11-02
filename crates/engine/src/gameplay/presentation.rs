//! Camera, lights and audio listener derived from the authored scene.

use engine_ecs::AudioListener;
use engine_ecs::components::{GlobalTransform, Transform as TransformComponent};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{Camera, LightSet};
use engine_scene::Scene;
use engine_utils::Transform;
use glam::{Mat3, Quat};

/// The scene's authored camera, or `live`, keeping the window's aspect ratio.
pub fn scene_camera(scene: &Scene, live: Camera) -> Camera {
    let mut camera = scene
        .entities
        .iter()
        .find_map(|entity| entity.camera.map(Camera::from))
        .unwrap_or(live);
    camera.aspect_ratio = live.aspect_ratio;
    camera
}

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
    let mut transform = Transform::from_translation(camera.eye);
    if camera.is_valid() {
        let pose = camera.view_matrix().inverse();
        transform.rotation = Quat::from_mat3(&Mat3::from_mat4(pose));
    }
    world
        .spawn((
            TransformComponent::from(transform),
            GlobalTransform::from(transform),
            AudioListener::default(),
        ))
        .id()
}
