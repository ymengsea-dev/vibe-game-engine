//! Which camera object the game views through, and the view it presents.
//!
//! Shared by the game runtime and Studio's Scene View (which culls as the
//! game camera would), so both pick the same camera the same way.

use engine_ecs::components::{
    ActiveCamera, Camera as CameraComponent, Disabled, GlobalTransform,
    Transform as TransformComponent,
};
use engine_ecs::prelude::{Entity, World};
use glam::Mat4;

use crate::camera_pose::camera_world_pose;

/// The camera entity the game views through.
///
/// `scene_order` is the scene's entities in scene order. Among those that are
/// still cameras, the first with [`ActiveCamera`] wins, else the first camera.
/// When none qualify, other camera entities (spawned at runtime) are ordered
/// by entity index, again preferring [`ActiveCamera`].
pub fn choose_active_camera(world: &mut World, scene_order: &[Entity]) -> Option<Entity> {
    // A disabled camera object is switched off; never view through it.
    let is_camera = |world: &World, entity: Entity| {
        world.get::<CameraComponent>(entity).is_some() && world.get::<Disabled>(entity).is_none()
    };
    let is_active = |world: &World, entity: Entity| world.get::<ActiveCamera>(entity).is_some();
    let listed: Vec<Entity> = scene_order
        .iter()
        .copied()
        .filter(|entity| is_camera(world, *entity))
        .collect();
    let candidates = if listed.is_empty() {
        let mut others: Vec<Entity> = world
            .query::<(Entity, &CameraComponent)>()
            .iter(world)
            .map(|(entity, _)| entity)
            .filter(|entity| is_camera(world, *entity))
            .collect();
        others.sort_by_key(|entity| entity.index());
        others
    } else {
        listed
    };
    candidates
        .iter()
        .copied()
        .find(|entity| is_active(world, *entity))
        .or_else(|| candidates.first().copied())
}

/// The renderer camera for a camera object whose world matrix is `world`:
/// eye, target and up from the matrix, everything else from `camera`.
#[must_use]
pub fn view_from_world(
    camera: &engine_renderer::Camera,
    world: Mat4,
    aspect: f32,
) -> engine_renderer::Camera {
    let (eye, target, up) = camera_world_pose(world);
    engine_renderer::Camera {
        eye,
        target,
        up,
        aspect_ratio: aspect,
        ..*camera
    }
}

/// The view `entity` presents, or `None` when it is not a camera. An object
/// with no transform keeps the camera component's own view.
#[must_use]
pub fn game_camera_view(
    world: &World,
    entity: Entity,
    aspect: f32,
) -> Option<engine_renderer::Camera> {
    let camera = world.get::<CameraComponent>(entity)?.0;
    let matrix = world
        .get::<GlobalTransform>(entity)
        .map(|global| global.0.to_matrix())
        .or_else(|| {
            world
                .get::<TransformComponent>(entity)
                .map(|local| local.0.to_matrix())
        });
    Some(match matrix {
        Some(matrix) => view_from_world(&camera, matrix, aspect),
        None => engine_renderer::Camera {
            aspect_ratio: aspect,
            ..camera
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn cam() -> engine_renderer::Camera {
        engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.5)
    }

    #[test]
    fn a_disabled_active_camera_is_skipped() {
        let mut world = World::new();
        let a = world.spawn(CameraComponent(cam())).id();
        let b = world
            .spawn((CameraComponent(cam()), ActiveCamera, Disabled))
            .id();
        assert_eq!(choose_active_camera(&mut world, &[a, b]), Some(a));
    }

    #[test]
    fn the_view_comes_from_the_object_transform() {
        let mut world = World::new();
        let entity = world
            .spawn((
                TransformComponent::from(engine_utils::Transform::from_translation(Vec3::new(
                    2.0, 1.0, 5.0,
                ))),
                CameraComponent(cam()),
            ))
            .id();
        let view = game_camera_view(&world, entity, 2.0).expect("a camera");
        assert!((view.eye - Vec3::new(2.0, 1.0, 5.0)).length() < 1e-4);
        assert!((view.target - Vec3::new(2.0, 1.0, 4.0)).length() < 1e-4);
        assert_eq!(view.aspect_ratio, 2.0);
    }

    #[test]
    fn a_camera_without_a_transform_keeps_its_own_view() {
        let mut world = World::new();
        let entity = world.spawn(CameraComponent(cam())).id();
        let view = game_camera_view(&world, entity, 3.0).expect("a camera");
        assert_eq!(view.eye, Vec3::ZERO);
        assert_eq!(view.aspect_ratio, 3.0);
        let other = world.spawn(TransformComponent::default()).id();
        assert!(game_camera_view(&world, other, 1.0).is_none());
    }
}
