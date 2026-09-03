//! Live `bevy_ecs::World` ↔ [`Scene`] conversion.
//!
//! [`Scene::from_world`] captures a world's entities into the serializable
//! [`Scene`] shape; [`Scene::instantiate`] spawns a scene's entities into
//! a world and wires up the parent/child hierarchy from the index
//! references. Together they are what the editor's "Save Scene" / "Open
//! Scene" actions use.
//!
//! Only `Name`, `Transform`, `Camera`, and the `ChildOf` parent link are
//! carried — the component set the editor currently creates.
//! `MeshRenderer`/`Sprite` need asset-handle resolution this conversion
//! has no access to (a `RenderAssets` + GPU); round-tripping those is
//! future work, tracked with the drag-into-scene feature.

use std::collections::HashMap;

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::{Entity, World};
use engine_ecs::components::{AssetSource, Camera, Disabled, Lock, Name, Static, Transform};

use crate::format::{CameraData, Scene, SceneEntity, TransformData};

impl Scene {
    /// Captures every **named** entity in `world` — its `Name`, plus its
    /// `Transform`/`Camera` if present, plus its parent (as an index into
    /// the returned scene's `entities`) if it has a `ChildOf`.
    ///
    /// Unnamed entities are skipped: it matches the editor hierarchy
    /// panel's "a root needs a name to show up" rule, and keeps
    /// `bevy_ecs`'s internal bookkeeping entities out of the file.
    /// Entities are ordered by name (a stable, diff-friendly result that
    /// doesn't depend on `Entity` internals); same-named entities keep
    /// their `World` iteration order.
    pub fn from_world(world: &mut World) -> Self {
        let mut query = world.query::<(
            Entity,
            &Name,
            Option<&Transform>,
            Option<&Camera>,
            Option<&AssetSource>,
            Option<&Disabled>,
            Option<&Static>,
        )>();
        let mut collected: Vec<(Entity, SceneEntity)> = query
            .iter(world)
            .map(
                |(entity, name, transform, camera, asset_source, disabled, is_static)| {
                    (
                        entity,
                        SceneEntity {
                            name: Some(name.0.clone()),
                            transform: transform.map(|t| TransformData::from(t.0)),
                            camera: camera.map(|c| CameraData::from(c.0)),
                            asset_source: asset_source.map(|source| source.path.clone()),
                            asset_id: asset_source.and_then(|source| source.id.clone()),
                            disabled: disabled.is_some(),
                            is_static: is_static.is_some(),
                            locked: false,
                            ..Default::default()
                        },
                    )
                },
            )
            .collect();
        // By name, not `Entity` order: neither `Entity`'s `Ord` nor its
        // `to_bits` tracks spawn order (both are niche-optimized), and a
        // name-sorted file diffs cleanly. `sort_by` is stable, so equal
        // names keep their iteration order.
        // `Lock` is read here, not in the query tuple above (kept small).
        for (entity, scene_entity) in &mut collected {
            scene_entity.locked = world.get::<Lock>(*entity).is_some();
        }

        collected.sort_by(|(_, a), (_, b)| a.name.cmp(&b.name));

        let index_of: HashMap<Entity, usize> = collected
            .iter()
            .enumerate()
            .map(|(index, (entity, _))| (*entity, index))
            .collect();

        let entities = collected
            .into_iter()
            .map(|(entity, mut scene_entity)| {
                // Only record a parent that is itself in the scene (i.e.
                // named); a child of an unnamed entity becomes a root.
                scene_entity.parent = world
                    .get::<ChildOf>(entity)
                    .and_then(|child_of| index_of.get(&child_of.parent()).copied());
                scene_entity
            })
            .collect();

        Self {
            version: crate::CURRENT_SCENE_VERSION,
            entities,
        }
    }

    /// Spawns this scene's entities into `world` and returns them in
    /// `entities`-index order, so `result[i]` is the entity for
    /// `self.entities[i]`.
    ///
    /// Runs in two passes: spawn every entity with its `Name`/`Transform`/
    /// `Camera`, then insert `ChildOf` for each one whose `parent` index
    /// is valid. An out-of-range or self-referential `parent` is ignored
    /// (the entity stays a root) rather than panicking — treat scene data
    /// as untrusted, same stance as [`Scene::from_ron_str`]. Call
    /// [`Scene::validate`] first to reject such a scene outright instead.
    pub fn instantiate(&self, world: &mut World) -> Vec<Entity> {
        let spawned: Vec<Entity> = self
            .entities
            .iter()
            .map(|scene_entity| {
                let mut entity_mut = world.spawn_empty();
                if let Some(name) = &scene_entity.name {
                    entity_mut.insert(Name::new(name.clone()));
                }
                if let Some(transform) = scene_entity.transform {
                    entity_mut.insert(Transform::from(engine_utils::Transform::from(transform)));
                }
                if let Some(camera) = scene_entity.camera {
                    entity_mut.insert(Camera::from(engine_renderer::Camera::from(camera)));
                }
                if scene_entity.asset_source.is_some() || scene_entity.asset_id.is_some() {
                    entity_mut.insert(AssetSource {
                        path: scene_entity.asset_source.clone().unwrap_or_default(),
                        id: scene_entity.asset_id.clone(),
                    });
                }
                if scene_entity.disabled {
                    entity_mut.insert(Disabled);
                }
                if scene_entity.is_static {
                    entity_mut.insert(Static);
                }
                if scene_entity.locked {
                    entity_mut.insert(Lock);
                }
                entity_mut.id()
            })
            .collect();

        for (index, scene_entity) in self.entities.iter().enumerate() {
            if let Some(parent_index) = scene_entity.parent
                && parent_index != index
                && let Some(&parent) = spawned.get(parent_index)
            {
                world.entity_mut(spawned[index]).insert(ChildOf(parent));
            }
        }

        spawned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    #[test]
    fn from_world_captures_named_entities_with_transforms() {
        let mut world = World::new();
        world.spawn((
            Name::new("Cube"),
            Transform::from(MathTransform::from_translation(Vec3::new(1.0, 2.0, 3.0))),
        ));

        let scene = Scene::from_world(&mut world);
        assert_eq!(scene.entities.len(), 1);
        assert_eq!(scene.entities[0].name.as_deref(), Some("Cube"));
        assert_eq!(
            scene.entities[0].transform.unwrap().translation,
            [1.0, 2.0, 3.0]
        );
        assert_eq!(scene.entities[0].parent, None);
    }

    #[test]
    fn from_world_skips_unnamed_entities() {
        let mut world = World::new();
        world.spawn(Transform::from(MathTransform::IDENTITY));
        world.spawn(Name::new("Kept"));

        let scene = Scene::from_world(&mut world);
        assert_eq!(scene.entities.len(), 1);
        assert_eq!(scene.entities[0].name.as_deref(), Some("Kept"));
    }

    #[test]
    fn from_world_records_parent_as_an_index() {
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        world.spawn((Name::new("Child"), ChildOf(parent)));

        let scene = Scene::from_world(&mut world);
        // Sorted by name: "Child" is index 0, "Parent" is index 1.
        assert_eq!(scene.entities[0].name.as_deref(), Some("Child"));
        assert_eq!(scene.entities[1].name.as_deref(), Some("Parent"));
        assert_eq!(scene.entities[0].parent, Some(1));
    }

    #[test]
    fn instantiate_then_from_world_round_trips() {
        let mut source = World::new();
        let parent = source.spawn(Name::new("Parent")).id();
        source.spawn((
            Name::new("Child"),
            Transform::from(MathTransform::from_scale(Vec3::splat(2.0))),
            ChildOf(parent),
        ));
        let scene = Scene::from_world(&mut source);

        let mut target = World::new();
        let spawned = scene.instantiate(&mut target);
        assert_eq!(spawned.len(), 2);

        let round_tripped = Scene::from_world(&mut target);
        assert_eq!(round_tripped, scene);
    }

    #[test]
    fn asset_source_round_trips() {
        let mut source = World::new();
        source.spawn((Name::new("Barrel"), AssetSource::new("props/barrel.gltf")));
        let scene = Scene::from_world(&mut source);
        assert_eq!(
            scene.entities[0].asset_source.as_deref(),
            Some("props/barrel.gltf")
        );
        assert_eq!(scene.entities[0].asset_id, None);

        let mut target = World::new();
        let spawned = scene.instantiate(&mut target);
        assert_eq!(
            target
                .get::<AssetSource>(spawned[0])
                .map(|s| s.path.as_str()),
            Some("props/barrel.gltf")
        );
    }

    #[test]
    fn asset_id_round_trips_and_old_scenes_still_parse() {
        let mut source = World::new();
        source.spawn((
            Name::new("Barrel"),
            AssetSource::with_id("props/barrel.gltf", "3fa00000-0000-0000-0000-000000000000"),
        ));
        let scene = Scene::from_world(&mut source);
        assert_eq!(
            scene.entities[0].asset_id.as_deref(),
            Some("3fa00000-0000-0000-0000-000000000000")
        );

        // Full round-trip keeps both fields.
        let ron = scene.to_ron_string().expect("serialize");
        let parsed = Scene::from_ron_str(&ron).expect("parse");
        let mut target = World::new();
        let spawned = parsed.instantiate(&mut target);
        let restored = target.get::<AssetSource>(spawned[0]).expect("AssetSource");
        assert_eq!(restored.path, "props/barrel.gltf");
        assert_eq!(
            restored.id.as_deref(),
            Some("3fa00000-0000-0000-0000-000000000000")
        );

        // A scene file written before `asset_id` existed (only
        // `asset_source`) still parses, with `asset_id` defaulting to None.
        let legacy = r#"(version: 1, entities: [(name: Some("Barrel"), asset_source: Some("props/barrel.gltf"))])"#;
        let parsed = Scene::from_ron_str(legacy).expect("legacy scene parses");
        assert_eq!(
            parsed.entities[0].asset_source.as_deref(),
            Some("props/barrel.gltf")
        );
        assert_eq!(parsed.entities[0].asset_id, None);
    }

    #[test]
    fn disabled_and_static_round_trip_and_default_off() {
        let mut source = World::new();
        source.spawn((Name::new("Plain"), Transform::default()));
        source.spawn((Name::new("Static"), Transform::default(), Static));
        source.spawn((Name::new("Zzz"), Transform::default(), Disabled));
        let scene = Scene::from_world(&mut source);

        // Name-sorted: "Plain", "Static", "Zzz".
        assert!(!scene.entities[0].disabled && !scene.entities[0].is_static);
        assert!(scene.entities[1].is_static && !scene.entities[1].disabled);
        assert!(scene.entities[2].disabled && !scene.entities[2].is_static);

        let mut target = World::new();
        let spawned = scene.instantiate(&mut target);
        assert!(target.get::<Static>(spawned[0]).is_none());
        assert!(target.get::<Static>(spawned[1]).is_some());
        assert!(target.get::<Disabled>(spawned[2]).is_some());

        assert_eq!(Scene::from_world(&mut target), scene);
    }

    #[test]
    fn locked_round_trips_and_defaults_off() {
        let mut source = World::new();
        source.spawn((Name::new("Free"), Transform::default()));
        source.spawn((Name::new("Pinned"), Transform::default(), Lock));
        let scene = Scene::from_world(&mut source);

        assert!(!scene.entities[0].locked);
        assert!(scene.entities[1].locked);

        let mut target = World::new();
        let spawned = scene.instantiate(&mut target);
        assert!(target.get::<Lock>(spawned[0]).is_none());
        assert!(target.get::<Lock>(spawned[1]).is_some());
        assert_eq!(Scene::from_world(&mut target), scene);
    }

    #[test]
    fn instantiate_ignores_an_out_of_range_parent_index() {
        let scene = Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: vec![SceneEntity {
                name: Some("Orphan".to_string()),
                parent: Some(99),
                ..Default::default()
            }],
        };
        let mut world = World::new();
        let spawned = scene.instantiate(&mut world);
        assert_eq!(spawned.len(), 1);
        assert!(world.get::<ChildOf>(spawned[0]).is_none());
    }

    #[test]
    fn instantiate_ignores_a_self_referential_parent() {
        let scene = Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: vec![SceneEntity {
                name: Some("Loop".to_string()),
                parent: Some(0),
                ..Default::default()
            }],
        };
        let mut world = World::new();
        let spawned = scene.instantiate(&mut world);
        assert!(world.get::<ChildOf>(spawned[0]).is_none());
    }
}
