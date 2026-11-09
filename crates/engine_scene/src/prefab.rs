//! Prefabs: reusable entity templates, instantiable into a live
//! `bevy_ecs::World` any number of times.
//!
//! A [`Prefab`] wraps the same [`SceneEntity`] data a scene file
//! describes — no new format, just a different use of the existing one:
//! a scene is "these entities, once"; a prefab is "this entity template,
//! stamp out as many copies as needed."
//!
//! A prefab holds one top object and any objects under it. A file from before
//! children existed is simply a prefab with none.

use std::collections::HashMap;
use std::path::Path;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::{EntityWorldMut, World};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::format::{Scene, SceneEntity};
use crate::resolve::{NullResolver, SceneResolver};

/// A reusable entity template.
///
/// Serialized form is a `.prefab` RON file — the same shape as one
/// [`SceneEntity`] in a scene, wrapped so the file is self-describing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefab {
    entity: SceneEntity,
    /// The objects under `entity`. Each `parent` indexes the list made of
    /// `entity` followed by these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    children: Vec<SceneEntity>,
}

impl Prefab {
    /// Wraps `entity`'s data as a prefab.
    pub fn new(entity: SceneEntity) -> Self {
        Self {
            entity,
            children: Vec::new(),
        }
    }

    /// A prefab from a top object followed by the objects under it; `parent`
    /// indexes this same list. `None` for an empty list.
    pub fn from_entities(entities: Vec<SceneEntity>) -> Option<Self> {
        let mut entities = entities.into_iter();
        let mut entity = entities.next()?;
        entity.parent = None;
        Some(Self {
            entity,
            children: entities.collect(),
        })
    }

    /// A prefab from captured objects, in any order, with `root_id` as the top
    /// object. `None` if it is not among them. Links to other prefabs are not
    /// kept: a prefab inside a prefab becomes plain objects.
    pub fn from_captured(mut entities: Vec<SceneEntity>, root_id: &str) -> Option<Self> {
        let root = entities
            .iter()
            .position(|entity| entity.object_id.as_deref() == Some(root_id))?;
        // Moving the root to the front shifts everything before it by one.
        let moved = |index: usize| match index {
            index if index == root => 0,
            index if index < root => index + 1,
            index => index,
        };
        let top = entities.remove(root);
        entities.insert(0, top);
        for entity in &mut entities {
            entity.prefab = None;
            entity.open_from = None;
            entity.locked = false;
            entity.parent = entity.parent.map(moved);
        }
        Self::from_entities(entities)
    }

    /// The top object, then the objects under it.
    pub fn entities(&self) -> impl Iterator<Item = &SceneEntity> {
        std::iter::once(&self.entity).chain(&self.children)
    }

    /// New scene objects from this prefab: each with a new ID, references
    /// between them pointed at the copies, and the top object first. With
    /// `link_path` they are linked to the prefab at that path.
    pub fn copy_entities(&self, link_path: Option<&str>) -> Vec<SceneEntity> {
        let mut new_ids = HashMap::new();
        let mut copies: Vec<SceneEntity> = self.entities().cloned().collect();
        for (index, copy) in copies.iter_mut().enumerate() {
            let member = crate::prefab_link::member_key(index, copy);
            let new_id = uuid::Uuid::new_v4().to_string();
            copy.prefab = link_path.map(|path| crate::PrefabLink {
                path: path.to_owned(),
                member,
                overrides: Vec::new(),
            });
            if let Some(old_id) = copy.object_id.replace(new_id.clone()) {
                new_ids.insert(old_id, new_id);
            }
        }
        for copy in &mut copies {
            crate::prefab_link::remap_object_references(copy, &new_ids);
        }
        copies
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
        let mut prefab: Self =
            ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))?;
        // Prefabs written before a camera's transform became its pose carry
        // eye/target/up only; fold them into the transform like a scene does.
        let mut holder = Scene {
            entities: std::iter::once(std::mem::take(&mut prefab.entity))
                .chain(std::mem::take(&mut prefab.children))
                .collect(),
            ..Default::default()
        };
        crate::adopt_legacy_camera_poses(&mut holder);
        Ok(Self::from_entities(holder.entities).unwrap_or_default())
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
        self.validate()?;
        let text = self.to_ron_string()?;
        std::fs::write(path, text).map_err(|err| SceneError::Io(err.to_string()))
    }

    /// Checks every object, and that each child's `parent` is in bounds and
    /// leads back to the top object.
    ///
    /// # Errors
    ///
    /// [`SceneError::Validation`] naming the first problem.
    pub fn validate(&self) -> Result<(), SceneError> {
        self.entity
            .validate()
            .map_err(|reason| SceneError::Validation(format!("prefab entity: {reason}")))?;
        if self.children.is_empty() {
            return Ok(());
        }
        Scene {
            entities: self.entities().cloned().collect(),
            ..Default::default()
        }
        .validate()
    }

    /// Validates, then writes atomically, for replacing a prefab in place.
    ///
    /// # Errors
    ///
    /// As [`Prefab::save_to_file`].
    pub fn save_atomic(&self, path: &Path) -> Result<(), SceneError> {
        self.validate()?;
        let text = self.to_ron_string()?;
        crate::write_atomic(path, text.as_bytes()).map_err(|err| SceneError::Io(err.to_string()))
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
    /// The objects under the top object spawn with it.
    pub fn instantiate_with_resolver(
        &self,
        world: &mut World,
        resolver: &mut impl SceneResolver,
        configure: impl FnOnce(&mut EntityWorldMut<'_>),
    ) -> (Entity, usize) {
        let mut unresolved = 0;
        let copies = self.copy_entities(None);
        let spawned = crate::world::spawn_scene_entities(&copies, world, resolver, &mut unresolved);
        let top = spawned
            .first()
            .copied()
            .unwrap_or_else(|| world.spawn_empty().id());
        configure(&mut world.entity_mut(top));
        (top, unresolved)
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
mod child_tests {
    use super::*;

    const TOP: &str = "00000000-0000-4000-8000-000000000001";
    const KID: &str = "00000000-0000-4000-8000-000000000002";

    fn object(name: &str, id: &str, parent: Option<usize>) -> SceneEntity {
        SceneEntity {
            name: Some(name.into()),
            object_id: Some(id.into()),
            parent,
            ..Default::default()
        }
    }

    #[test]
    fn a_file_from_before_children_still_loads() {
        let text = "(entity: (name: Some(\"Old\")))";
        let prefab = Prefab::from_ron_str(text).unwrap();
        assert_eq!(prefab.entities().count(), 1);
    }

    #[test]
    fn captured_objects_put_the_top_object_first_and_keep_parents_right() {
        // Sorted by name, as captures are: the child comes first.
        let mut kid = object("A Kid", KID, Some(1));
        kid.locked = true;
        let mut top = object("Top", TOP, None);
        top.prefab = Some(crate::PrefabLink::default());
        let prefab = Prefab::from_captured(vec![kid, top], TOP).unwrap();
        let list: Vec<_> = prefab.entities().collect();
        assert_eq!(list[0].name.as_deref(), Some("Top"));
        assert_eq!(list[1].parent, Some(0));
        assert!(list.iter().all(|e| e.prefab.is_none() && !e.locked));
        assert!(Prefab::from_captured(vec![object("x", KID, None)], TOP).is_none());
    }

    #[test]
    fn children_spawn_under_the_top_object_with_new_ids() {
        let prefab =
            Prefab::from_entities(vec![object("Top", TOP, None), object("Kid", KID, Some(0))])
                .unwrap();
        let text = prefab.to_ron_string().unwrap();
        let prefab = Prefab::from_ron_str(&text).unwrap();
        let mut world = World::new();
        let top = prefab.instantiate(&mut world);
        let kids = world
            .get::<bevy_ecs::hierarchy::Children>(top)
            .expect("the child came too");
        assert_eq!(kids.len(), 1);
        let kid = kids[0];
        assert_ne!(world.get::<crate::SceneObjectId>(kid).unwrap().0, KID);
        assert_ne!(world.get::<crate::SceneObjectId>(top).unwrap().0, TOP);
    }

    #[test]
    fn a_child_with_a_bad_parent_does_not_validate() {
        let prefab =
            Prefab::from_entities(vec![object("Top", TOP, None), object("Kid", KID, Some(9))])
                .unwrap();
        assert!(prefab.validate().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::TransformData;
    use engine_ecs::components::{Camera as EcsCamera, Name as EcsName, Transform as EcsTransform};

    fn moved_camera_entity() -> (World, Entity) {
        let mut world = World::new();
        let camera = engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 16.0 / 9.0);
        let entity = world
            .spawn((
                EcsName::new("Cam"),
                EcsTransform(
                    engine_utils::Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
                        .with_rotation(glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)),
                ),
                EcsCamera(camera),
                engine_ecs::components::ActiveCamera,
            ))
            .id();
        (world, entity)
    }

    #[test]
    fn captured_camera_data_carries_the_world_pose_and_active_flag() {
        let (world, entity) = moved_camera_entity();
        let data = crate::saved_camera_pose(
            &world,
            entity,
            crate::CameraData::from(world.get::<EcsCamera>(entity).unwrap().0),
        );
        assert!(data.pose_from_transform && data.active);
        let reloaded = Prefab::new(SceneEntity {
            camera: Some(data),
            transform: Some(TransformData::from(
                world.get::<EcsTransform>(entity).unwrap().0,
            )),
            ..Default::default()
        });
        let text = reloaded.to_ron_string().unwrap();
        let reloaded = Prefab::from_ron_str(&text).unwrap();
        let mut target = World::new();
        let spawned = reloaded.instantiate(&mut target);
        let (eye, look, _) =
            crate::camera_world_pose(target.get::<EcsTransform>(spawned).unwrap().0.to_matrix());
        assert!((eye - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-4);
        assert!((look - Vec3::new(0.0, 2.0, 3.0)).length() < 1e-4);
        assert!(
            target
                .get::<engine_ecs::components::ActiveCamera>(spawned)
                .is_some()
        );
    }

    #[test]
    fn legacy_prefab_camera_is_adopted_on_load() {
        let text = "(entity: (camera: Some((eye: (0.0, 2.0, 6.0), target: (0.0, 0.0, 0.0), \
                    up: (0.0, 1.0, 0.0), aspect_ratio: 1.5, \
                    projection: Perspective(fov_y_radians: 1.0), near: 0.1, far: 50.0))))";
        let prefab = Prefab::from_ron_str(text).unwrap();
        let entity = prefab.entity();
        assert!(entity.camera.unwrap().pose_from_transform);
        assert_eq!(entity.transform.unwrap().translation, [0.0, 2.0, 6.0]);
    }

    #[test]
    fn prefab_copies_receive_distinct_object_ids() {
        let prefab = Prefab::new(SceneEntity {
            object_id: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        });
        let mut world = World::new();
        let first = prefab.instantiate(&mut world);
        let second = prefab.instantiate(&mut world);
        assert_ne!(
            world.get::<crate::SceneObjectId>(first),
            world.get::<crate::SceneObjectId>(second)
        );
    }

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
        use engine_ecs::components::{AssetSource, Disabled};

        let prefab = Prefab::new(SceneEntity {
            name: Some("Rock".to_string()),
            asset_source: Some("props/rock.gltf".to_string()),
            asset_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            disabled: true,
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
    }
}
