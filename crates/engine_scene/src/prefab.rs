//! Prefabs: reusable entity templates, instantiable into a live
//! `bevy_ecs::World` any number of times.
//!
//! A [`Prefab`] wraps the same [`SceneEntity`] data a scene file
//! describes — no new format, just a different use of the existing one:
//! a scene is "these entities, once"; a prefab is "this entity template,
//! stamp out as many copies as needed."

use bevy_ecs::entity::Entity;
use bevy_ecs::world::{EntityWorldMut, World};
use glam::Vec3;

use crate::format::SceneEntity;

/// A reusable entity template.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prefab {
    entity: SceneEntity,
}

impl Prefab {
    /// Wraps `entity`'s data as a prefab.
    pub fn new(entity: SceneEntity) -> Self {
        Self { entity }
    }

    /// The template data this prefab instantiates from.
    pub fn entity(&self) -> &SceneEntity {
        &self.entity
    }

    /// Spawns a new entity in `world` with this prefab's components,
    /// unmodified.
    pub fn instantiate(&self, world: &mut World) -> Entity {
        self.instantiate_with(world, |_| {})
    }

    /// Spawns a new entity in `world` with this prefab's components, then
    /// runs `configure` on it before returning — the hook for per-instance
    /// overrides ("this copy, but at a different position").
    ///
    /// # Example
    ///
    /// ```
    /// use bevy_ecs::world::World;
    /// use engine_scene::{Prefab, SceneEntity, TransformData};
    ///
    /// let prefab = Prefab::new(SceneEntity {
    ///     transform: Some(TransformData {
    ///         translation: [0.0, 0.0, 0.0],
    ///         rotation: glam::Quat::IDENTITY.to_array(),
    ///         scale: [1.0, 1.0, 1.0],
    ///     }),
    ///     ..Default::default()
    /// });
    ///
    /// let mut world = World::new();
    /// let entity = prefab.instantiate_with(&mut world, |entity_mut| {
    ///     entity_mut.insert(engine_ecs::components::Transform::from(
    ///         engine_utils::Transform::from_translation(glam::Vec3::new(5.0, 0.0, 0.0)),
    ///     ));
    /// });
    /// let transform = world.get::<engine_ecs::components::Transform>(entity).unwrap();
    /// assert_eq!(transform.0.translation, glam::Vec3::new(5.0, 0.0, 0.0));
    /// ```
    pub fn instantiate_with(
        &self,
        world: &mut World,
        configure: impl FnOnce(&mut EntityWorldMut<'_>),
    ) -> Entity {
        let mut entity_mut = world.spawn_empty();

        if let Some(name) = &self.entity.name {
            entity_mut.insert(engine_ecs::components::Name::new(name.clone()));
        }
        if let Some(transform) = self.entity.transform {
            entity_mut.insert(engine_ecs::components::Transform::from(
                engine_utils::Transform::from(transform),
            ));
        }
        if let Some(camera) = self.entity.camera {
            entity_mut.insert(engine_ecs::components::Camera::from(
                engine_renderer::Camera::from(camera),
            ));
        }
        // `parent` and `mesh_renderer` aren't wired up here: `parent` is
        // only meaningful with the full sibling list a lone `Prefab`
        // doesn't have, and `mesh_renderer` needs GPU/asset resolution
        // this single-entity API doesn't have access to (Stage 3/4).

        configure(&mut entity_mut);
        entity_mut.id()
    }

    /// Spawns a new entity in `world` with this prefab's components, but
    /// with its transform's translation overridden to `translation`
    /// (rotation/scale unchanged, or identity if this prefab has no
    /// transform at all).
    ///
    /// Shorthand for the common "same prefab, different spot" case; use
    /// [`Prefab::instantiate_with`] directly for anything beyond just
    /// translation.
    pub fn instantiate_at(&self, world: &mut World, translation: Vec3) -> Entity {
        self.instantiate_with(world, |entity_mut| {
            let mut transform = entity_mut
                .get::<engine_ecs::components::Transform>()
                .map(|t| t.0)
                .unwrap_or_default();
            transform.translation = translation;
            entity_mut.insert(engine_ecs::components::Transform::from(transform));
        })
    }
}

impl From<SceneEntity> for Prefab {
    fn from(entity: SceneEntity) -> Self {
        Self::new(entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::TransformData;
    use engine_ecs::components::{Camera as EcsCamera, Name as EcsName, Transform as EcsTransform};

    fn transform_data_at(x: f32) -> TransformData {
        TransformData {
            translation: [x, 0.0, 0.0],
            rotation: glam::Quat::IDENTITY.to_array(),
            scale: [1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn instantiate_empty_prefab_spawns_bare_entity() {
        let prefab = Prefab::default();
        let mut world = World::new();
        let entity = prefab.instantiate(&mut world);

        assert!(world.get::<EcsTransform>(entity).is_none());
        assert!(world.get::<EcsCamera>(entity).is_none());
    }

    #[test]
    fn instantiate_applies_transform_component() {
        let prefab = Prefab::new(SceneEntity {
            transform: Some(transform_data_at(3.0)),
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate(&mut world);

        assert_eq!(
            world.get::<EcsTransform>(entity).unwrap().0.translation,
            Vec3::new(3.0, 0.0, 0.0)
        );
    }

    #[test]
    fn instantiate_applies_name_component() {
        let prefab = Prefab::new(SceneEntity {
            name: Some("Player".to_string()),
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate(&mut world);

        assert_eq!(world.get::<EcsName>(entity).unwrap().0, "Player");
    }

    #[test]
    fn instantiate_without_name_inserts_no_name_component() {
        let prefab = Prefab::new(SceneEntity {
            transform: Some(transform_data_at(0.0)),
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate(&mut world);

        assert!(world.get::<EcsName>(entity).is_none());
    }

    #[test]
    fn instantiate_twice_creates_two_independent_entities() {
        let prefab = Prefab::new(SceneEntity {
            transform: Some(transform_data_at(1.0)),
            ..Default::default()
        });
        let mut world = World::new();
        let a = prefab.instantiate(&mut world);
        let b = prefab.instantiate(&mut world);

        assert_ne!(a, b);
        world.get_mut::<EcsTransform>(a).unwrap().0.translation.x = 99.0;
        assert_eq!(
            world.get::<EcsTransform>(b).unwrap().0.translation.x,
            1.0,
            "mutating one instance must not affect the other"
        );
    }

    #[test]
    fn instantiate_at_overrides_translation_only() {
        let prefab = Prefab::new(SceneEntity {
            transform: Some(TransformData {
                translation: [1.0, 2.0, 3.0],
                rotation: glam::Quat::from_rotation_y(0.5).to_array(),
                scale: [2.0, 2.0, 2.0],
            }),
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate_at(&mut world, Vec3::new(9.0, 9.0, 9.0));

        let transform = world.get::<EcsTransform>(entity).unwrap().0;
        assert_eq!(transform.translation, Vec3::new(9.0, 9.0, 9.0));
        assert_eq!(transform.rotation, glam::Quat::from_rotation_y(0.5));
        assert_eq!(transform.scale, Vec3::splat(2.0));
    }

    #[test]
    fn instantiate_at_on_transformless_prefab_still_sets_translation() {
        let prefab = Prefab::default();
        let mut world = World::new();
        let entity = prefab.instantiate_at(&mut world, Vec3::new(4.0, 5.0, 6.0));

        assert_eq!(
            world.get::<EcsTransform>(entity).unwrap().0.translation,
            Vec3::new(4.0, 5.0, 6.0)
        );
    }

    #[test]
    fn instantiate_with_lets_caller_add_extra_overrides() {
        let prefab = Prefab::new(SceneEntity {
            transform: Some(transform_data_at(0.0)),
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate_with(&mut world, |entity_mut| {
            entity_mut.insert(EcsTransform::from(engine_utils::Transform::from_scale(
                Vec3::splat(3.0),
            )));
        });

        assert_eq!(
            world.get::<EcsTransform>(entity).unwrap().0.scale,
            Vec3::splat(3.0)
        );
    }

    #[test]
    fn from_scene_entity_matches_new() {
        let data = SceneEntity {
            transform: Some(transform_data_at(7.0)),
            ..Default::default()
        };
        assert_eq!(Prefab::from(data.clone()), Prefab::new(data));
    }
}
