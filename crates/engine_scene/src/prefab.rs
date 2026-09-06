//! Prefabs: reusable entity templates, instantiable into a live
//! `bevy_ecs::World` any number of times.
//!
//! A [`Prefab`] wraps the same [`SceneEntity`] data a scene file
//! describes — no new format, just a different use of the existing one:
//! a scene is "these entities, once"; a prefab is "this entity template,
//! stamp out as many copies as needed."

use std::path::Path;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::{EntityWorldMut, World};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::format::SceneEntity;
use crate::resolve::{NullResolver, SceneResolver, insert_components};

/// A reusable entity template.
///
/// Serialized form is a `.prefab` RON file — the same shape as one
/// [`SceneEntity`] in a scene, wrapped so the file is self-describing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

    /// Serializes this prefab to pretty-printed RON text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Serialize`] if RON encoding fails.
    pub fn to_ron_string(&self) -> Result<String, SceneError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| SceneError::Serialize(err.to_string()))
    }

    /// Parses a prefab from RON text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Deserialize`] if `text` isn't valid RON matching a
    /// [`Prefab`].
    pub fn from_ron_str(text: &str) -> Result<Self, SceneError> {
        ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))
    }

    /// Validates the template, then writes it to `path` as RON,
    /// overwriting any existing file.
    ///
    /// # Errors
    ///
    /// [`SceneError::Validation`] if the template has invalid component
    /// data (a non-finite transform, a degenerate camera, ...);
    /// [`SceneError::Serialize`] if RON encoding fails;
    /// [`SceneError::Io`] if the write fails.
    pub fn save_to_file(&self, path: &Path) -> Result<(), SceneError> {
        self.entity
            .validate()
            .map_err(|reason| SceneError::Validation(format!("prefab entity: {reason}")))?;
        let text = self.to_ron_string()?;
        std::fs::write(path, text).map_err(|err| SceneError::Io(err.to_string()))
    }

    /// Reads and parses a prefab from `path`. The file is untrusted
    /// input — malformed RON is rejected rather than propagated.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] if the file can't be read;
    /// [`SceneError::Deserialize`] if its contents aren't a valid
    /// [`Prefab`].
    pub fn load_from_file(path: &Path) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path).map_err(|err| SceneError::Io(err.to_string()))?;
        Self::from_ron_str(&text)
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
        self.instantiate_with_resolver(world, &mut NullResolver, configure)
            .0
    }

    /// Spawns this prefab's entity as [`Prefab::instantiate_with`] does,
    /// additionally resolving its mesh/sprite asset references through
    /// `resolver` into live renderable components.
    ///
    /// Returns the spawned entity and how many of its references failed
    /// to resolve (`0` when everything resolved or nothing needed
    /// resolving). A failed reference is logged and skipped, never fatal —
    /// same contract as [`crate::Scene::instantiate_with_resolver`], which
    /// shares this method's implementation.
    ///
    /// `parent` is still not wired up: it indexes into an owning
    /// [`crate::Scene`]'s entity list, which a lone prefab has no
    /// equivalent of.
    pub fn instantiate_with_resolver(
        &self,
        world: &mut World,
        resolver: &mut impl SceneResolver,
        configure: impl FnOnce(&mut EntityWorldMut<'_>),
    ) -> (Entity, usize) {
        let mut unresolved = 0;
        let mut entity_mut = world.spawn_empty();
        insert_components(&mut entity_mut, &self.entity, resolver, &mut unresolved);
        configure(&mut entity_mut);
        (entity_mut.id(), unresolved)
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

    fn temp_prefab_path(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "vge-engine_scene-prefab-{name}-{}-{n}.prefab",
            std::process::id()
        ))
    }

    #[test]
    fn prefab_ron_round_trips() {
        let prefab = Prefab::new(SceneEntity {
            name: Some("Crate".to_string()),
            transform: Some(transform_data_at(2.0)),
            asset_source: Some("props/crate.gltf".to_string()),
            asset_id: Some("3fa00000-0000-0000-0000-000000000000".to_string()),
            is_static: true,
            ..Default::default()
        });
        let text = prefab.to_ron_string().unwrap();
        assert_eq!(Prefab::from_ron_str(&text).unwrap(), prefab);
    }

    #[test]
    fn save_then_load_round_trips_on_disk() {
        let path = temp_prefab_path("round-trip");
        let prefab = Prefab::new(SceneEntity {
            name: Some("Barrel".to_string()),
            transform: Some(transform_data_at(1.0)),
            ..Default::default()
        });
        prefab.save_to_file(&path).unwrap();
        let loaded = Prefab::load_from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, prefab);
    }

    #[test]
    fn save_to_file_rejects_a_nan_transform_without_touching_disk() {
        let path = temp_prefab_path("nan");
        let prefab = Prefab::new(SceneEntity {
            transform: Some(TransformData {
                translation: [f32::NAN, 0.0, 0.0],
                rotation: glam::Quat::IDENTITY.to_array(),
                scale: [1.0, 1.0, 1.0],
            }),
            ..Default::default()
        });
        let err = prefab.save_to_file(&path).unwrap_err();
        assert!(matches!(err, SceneError::Validation(_)));
        assert!(!path.exists());
    }

    #[test]
    fn load_from_file_rejects_malformed_ron() {
        let path = temp_prefab_path("malformed");
        std::fs::write(&path, "not a prefab {{{").unwrap();
        let err = Prefab::load_from_file(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(matches!(err, SceneError::Deserialize(_)));
    }

    #[test]
    fn load_from_file_missing_returns_io_error() {
        let err = Prefab::load_from_file(&temp_prefab_path("missing")).unwrap_err();
        assert!(matches!(err, SceneError::Io(_)));
    }

    #[test]
    fn instantiate_inserts_asset_source_and_markers() {
        use engine_ecs::components::{AssetSource, Disabled, Static};

        let prefab = Prefab::new(SceneEntity {
            name: Some("Rock".to_string()),
            asset_source: Some("props/rock.gltf".to_string()),
            asset_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            disabled: true,
            is_static: true,
            ..Default::default()
        });
        let mut world = World::new();
        let entity = prefab.instantiate(&mut world);

        let source = world.get::<AssetSource>(entity).unwrap();
        assert_eq!(source.path, "props/rock.gltf");
        assert_eq!(
            source.id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert!(world.get::<Disabled>(entity).is_some());
        assert!(world.get::<Static>(entity).is_some());
    }
}
