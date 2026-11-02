//! Live `bevy_ecs::World` ↔ [`Scene`] conversion.
//!
//! [`Scene::from_world`] captures a world's entities into the serializable
//! [`Scene`] shape; [`Scene::instantiate`] spawns a scene's entities into
//! a world and wires up the parent/child hierarchy from the index
//! references. Together they are what the editor's "Save Scene" / "Open
//! Scene" actions use.
//!
//! Both directions carry renderables. A live `MeshRenderer` holds GPU
//! handles rather than asset ids, so the ids travel on sibling carrier
//! components (`MeshSource`/`SpriteSource`) that
//! [`Scene::instantiate_with_resolver`] stamps on spawn and
//! [`Scene::from_world`] reads back via [`capture_renderables`]. A
//! reference that failed to resolve still round-trips, so re-saving a
//! scene whose asset file is missing never deletes the reference.

use std::collections::{HashMap, HashSet};

use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::prelude::{Entity, World};
use engine_ecs::components::{
    AssetSource, Camera, Disabled, Lock, Name, Script, Static, Transform,
};

use crate::capture::capture_renderables;
use crate::format::{CameraData, Scene, SceneEntity, TransformData};
use crate::resolve::{InstantiateReport, NullResolver, SceneResolver, insert_components};

/// Resolves authored point lights against current parent-relative transforms.
pub fn point_lights_from_world(world: &mut World) -> Vec<engine_renderer::PointLight> {
    engine_ecs::propagate_transforms(world);
    let mut query = world.query_filtered::<(
        &engine_ecs::components::GlobalTransform,
        &crate::format::ScenePointLight,
    ), bevy_ecs::prelude::Without<Disabled>>();
    query
        .iter(world)
        .map(|(transform, light)| engine_renderer::PointLight {
            position: transform.0.translation,
            color: glam::Vec3::from_array(light.0.color),
            intensity: light.0.intensity,
            range: light.0.range,
        })
        .collect()
}

impl Scene {
    /// Gives legacy entities an in-memory identity before scripts run.
    /// The editor persists these IDs on its next scene save.
    pub fn assign_missing_object_ids(&mut self) {
        let mut used: HashSet<String> = self
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.clone())
            .collect();
        for entity in &mut self.entities {
            if entity.object_id.is_none() {
                loop {
                    let id = uuid::Uuid::new_v4().to_string();
                    if used.insert(id.clone()) {
                        entity.object_id = Some(id);
                        break;
                    }
                }
            }
        }
    }

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
        // `Lock` and the renderable carriers are read here, not in the
        // query tuple above (kept small, and `capture_renderables` needs
        // `&World` while the query holds it). That helper is shared with
        // the editor's prefab capture so the two can't drift.
        for (entity, scene_entity) in &mut collected {
            scene_entity.sibling_order = world
                .get::<crate::format::SiblingOrder>(*entity)
                .map(|order| order.0);
            scene_entity.script = world
                .get::<Script>(*entity)
                .map(|script| script.path.clone());
            let object_id = match world.get::<crate::SceneObjectId>(*entity) {
                Some(id) => id.0.clone(),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    world
                        .entity_mut(*entity)
                        .insert(crate::SceneObjectId(id.clone()));
                    id
                }
            };
            scene_entity.object_id = Some(object_id);
            scene_entity.locked = world.get::<Lock>(*entity).is_some();
            scene_entity.collider = world
                .get::<crate::format::SceneCollider>(*entity)
                .map(|carrier| carrier.0.clone());
            scene_entity.audio_emitter = world
                .get::<crate::format::SceneAudioEmitter>(*entity)
                .map(|carrier| carrier.0.clone());
            scene_entity.water = world
                .get::<engine_ecs::components::WaterVolume>(*entity)
                .map(|volume| volume.surface.into());
            scene_entity.point_light = world
                .get::<crate::format::ScenePointLight>(*entity)
                .map(|light| light.0);
            scene_entity.ui = world
                .get::<crate::format::SceneUi>(*entity)
                .map(|ui| ui.0.clone());
            let (mesh_renderer, sprite) = capture_renderables(world, *entity);
            scene_entity.mesh_renderer = mesh_renderer;
            scene_entity.foliage = world
                .get::<crate::SceneFoliage>(*entity)
                .map(|source| source.0.clone());
            scene_entity.sprite = sprite;
            scene_entity.tilemap = world
                .get::<crate::SceneTilemap>(*entity)
                .map(|source| source.0.clone());
            scene_entity.rules = world
                .get::<crate::SceneRules>(*entity)
                .map_or_else(Vec::new, |source| source.0.clone());
        }

        // By name, not `Entity` order: neither `Entity`'s `Ord` nor its
        // `to_bits` tracks spawn order (both are niche-optimized), and a
        // name-sorted file diffs cleanly. `sort_by` is stable, so equal
        // names keep their iteration order.
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
            // Scene-level, so it rides a world resource rather than an
            // entity — same carrier reasoning, one level up.
            nav_grid: world
                .get_resource::<crate::format::SceneNavGrid>()
                .map(|carrier| carrier.0.clone()),
            lighting: world
                .get_resource::<crate::format::SceneLighting>()
                .map(|carrier| carrier.0),
            environment: world
                .get_resource::<crate::format::SceneEnvironment>()
                .map(|carrier| carrier.0.clone())
                .unwrap_or_default(),
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
        self.instantiate_with_resolver(world, &mut NullResolver)
            .spawned
    }

    /// Spawns this scene's entities into `world` exactly as
    /// [`Scene::instantiate`] does, additionally resolving each entity's
    /// mesh/sprite asset references through `resolver` into live
    /// renderable components.
    ///
    /// This is the method that makes a saved scene *visible* again:
    /// [`Scene::instantiate`] carries names, transforms, cameras, and
    /// hierarchy, but a `MeshRenderer` holds GPU handles that only an
    /// asset-owning caller can rebuild. See [`SceneResolver`].
    ///
    /// A reference that fails to resolve is logged, counted in
    /// [`InstantiateReport::unresolved`], and skipped — the entity still
    /// spawns with everything else intact. Scene data is untrusted; one
    /// broken asset never costs you the rest of the scene.
    pub fn instantiate_with_resolver(
        &self,
        world: &mut World,
        resolver: &mut impl SceneResolver,
    ) -> InstantiateReport {
        let mut unresolved = 0;
        // Scene-level data goes in before the entities, so a system that
        // reacts to a spawn can already see the level it spawned into.
        if let Some(nav_grid) = &self.nav_grid {
            world.insert_resource(crate::format::SceneNavGrid(nav_grid.clone()));
        }
        if let Some(lighting) = self.lighting {
            world.insert_resource(crate::format::SceneLighting(lighting));
        } else {
            world.remove_resource::<crate::format::SceneLighting>();
        }
        if self.environment.is_empty() {
            world.remove_resource::<crate::format::SceneEnvironment>();
        } else {
            world.insert_resource(crate::format::SceneEnvironment(self.environment.clone()));
        }
        let spawned: Vec<Entity> = self
            .entities
            .iter()
            .map(|scene_entity| {
                let mut entity_mut = world.spawn_empty();
                insert_components(&mut entity_mut, scene_entity, resolver, &mut unresolved);
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

        if unresolved > 0 {
            tracing::warn!(
                unresolved,
                entities = spawned.len(),
                "scene instantiated with unresolved asset references"
            );
        }

        InstantiateReport {
            spawned,
            unresolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::Script;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    #[test]
    fn authored_button_round_trips_and_mounts_as_clickable_screen_ui() {
        use crate::{SceneUi, UiAnchor, UiElementData, UiKind};
        use engine_ui::{PointerInput, Ui};

        let object_id = uuid::Uuid::new_v4().to_string();
        let mut world = World::new();
        world.spawn((
            Name::new("Start Button"),
            crate::SceneObjectId(object_id.clone()),
            SceneUi(UiElementData {
                kind: UiKind::Button,
                text: "Start".into(),
                anchor: UiAnchor::BottomRight,
                offset: [-20.0, -15.0],
                size: [100.0, 30.0],
                color: [0.2, 0.3, 0.4, 1.0],
            }),
        ));
        let text = Scene::from_world(&mut world).to_ron_string().unwrap();
        let reopened = Scene::from_ron_str(&text).unwrap();
        let mut restored = World::new();
        let entities = reopened.instantiate(&mut restored);
        assert_eq!(
            restored.get::<SceneUi>(entities[0]).unwrap().0.text,
            "Start"
        );

        let mut ui = Ui::new(400.0, 200.0);
        let buttons = reopened.mount_ui(&mut ui);
        let button = *buttons
            .iter()
            .find_map(|(node, id)| (id == &object_id).then_some(node))
            .expect("mounted button");
        ui.layout();
        let rect = ui.rect_of(button).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.w, rect.h),
            (280.0, 155.0, 100.0, 30.0)
        );
        ui.interact(PointerInput {
            position: [300.0, 170.0],
            pressed: true,
            released: false,
        });
        assert_eq!(
            ui.interact(PointerInput {
                position: [300.0, 170.0],
                pressed: false,
                released: true,
            }),
            vec![button]
        );
    }

    #[test]
    fn mounted_ui_maps_labels_and_omits_disabled_objects() {
        let mut scene = Scene::default();
        scene.entities.push(crate::SceneEntity {
            object_id: Some("label-id".into()),
            ui: Some(crate::UiElementData::label()),
            ..Default::default()
        });
        scene.entities.push(crate::SceneEntity {
            object_id: Some("disabled-id".into()),
            ui: Some(crate::UiElementData::button()),
            disabled: true,
            ..Default::default()
        });
        let mut ui = engine_ui::Ui::new(400.0, 200.0);
        let nodes = scene.mount_ui(&mut ui);
        assert_eq!(
            nodes.values().collect::<Vec<_>>(),
            vec![&"label-id".to_owned()]
        );
    }

    #[test]
    fn legacy_scene_assigns_missing_ids_once_without_changing_existing_ids() {
        let existing = uuid::Uuid::new_v4().to_string();
        let mut scene = Scene {
            entities: vec![
                SceneEntity {
                    name: Some("Old A".into()),
                    ..Default::default()
                },
                SceneEntity {
                    name: Some("Old B".into()),
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(existing.clone()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        scene.assign_missing_object_ids();
        let ids: Vec<_> = scene
            .entities
            .iter()
            .map(|entity| entity.object_id.clone())
            .collect();
        assert!(ids.iter().all(Option::is_some));
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[2].as_deref(), Some(existing.as_str()));
        scene.assign_missing_object_ids();
        assert_eq!(
            scene
                .entities
                .iter()
                .map(|entity| entity.object_id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }

    #[test]
    fn script_attachment_and_object_id_survive_scene_save_and_load() {
        let mut world = World::new();
        world.spawn((Name::new("Actor"), Script::new("assets/scripts/actor.lua")));
        let first = Scene::from_world(&mut world);
        assert_eq!(
            first.entities[0].script.as_deref(),
            Some("assets/scripts/actor.lua")
        );
        let id = first.entities[0]
            .object_id
            .as_ref()
            .expect("stable object ID");
        assert!(uuid::Uuid::parse_str(id).is_ok());
        assert_eq!(
            Scene::from_world(&mut world).entities[0].object_id.as_ref(),
            Some(id)
        );

        let mut restored = World::new();
        first.instantiate(&mut restored);
        assert_eq!(Scene::from_world(&mut restored), first);
    }

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
    fn sibling_order_round_trips_through_world_and_scene() {
        let mut source = World::new();
        source.spawn((Name::new("First"), crate::SiblingOrder(2)));
        let scene = Scene::from_world(&mut source);
        assert_eq!(scene.entities[0].sibling_order, Some(2));

        let mut restored = World::new();
        let entities = scene.instantiate(&mut restored);
        assert_eq!(
            restored.get::<crate::SiblingOrder>(entities[0]),
            Some(&crate::SiblingOrder(2))
        );
        assert_eq!(Scene::from_world(&mut restored), scene);
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
            nav_grid: None,
            lighting: None,
            environment: Default::default(),
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
            nav_grid: None,
            lighting: None,
            environment: Default::default(),
        };
        let mut world = World::new();
        let spawned = scene.instantiate(&mut world);
        assert!(world.get::<ChildOf>(spawned[0]).is_none());
    }

    // --- T-01: resolver-aware instantiation -------------------------

    fn scene_with_one_sprite_entity() -> Scene {
        Scene {
            entities: vec![SceneEntity {
                name: Some("Coin".into()),
                sprite: Some(crate::format::SpriteData {
                    atlas: crate::format::AssetRef {
                        id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
                    },
                    region: "coin_0".into(),
                    size: [1.0, 1.0],
                    color: [1.0, 1.0, 1.0, 1.0],
                    z_order: 0.0,
                    animation: None,
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn instantiate_with_resolver_inserts_resolved_renderable() {
        let scene = scene_with_one_sprite_entity();
        let mut world = World::new();
        let mut resolver = crate::resolve::test_support::StubResolver::resolving_sprites();

        let report = scene.instantiate_with_resolver(&mut world, &mut resolver);

        assert_eq!(report.spawned.len(), 1);
        assert!(report.is_fully_resolved());
        assert!(
            world
                .get::<engine_ecs::components::Sprite>(report.spawned[0])
                .is_some()
        );
    }

    #[test]
    fn instantiate_with_resolver_skips_unresolved_and_counts_it() {
        let scene = scene_with_one_sprite_entity();
        let mut world = World::new();
        let mut resolver = crate::resolve::test_support::StubResolver::failing();

        let report = scene.instantiate_with_resolver(&mut world, &mut resolver);

        assert_eq!(report.unresolved, 1);
        assert!(!report.is_fully_resolved());
        assert!(
            world
                .get::<engine_ecs::components::Sprite>(report.spawned[0])
                .is_none()
        );
        assert_eq!(
            world.get::<Name>(report.spawned[0]).map(|n| n.0.as_str()),
            Some("Coin"),
        );
    }

    #[test]
    fn broken_reference_does_not_abort_the_remaining_entities() {
        let mut scene = scene_with_one_sprite_entity();
        scene.entities.push(SceneEntity {
            name: Some("Plain".into()),
            transform: Some(TransformData {
                translation: [7.0, 0.0, 0.0],
                rotation: glam::Quat::IDENTITY.to_array(),
                scale: [1.0, 1.0, 1.0],
            }),
            ..Default::default()
        });
        let mut world = World::new();
        let mut resolver = crate::resolve::test_support::StubResolver::failing();

        let report = scene.instantiate_with_resolver(&mut world, &mut resolver);

        assert_eq!(report.spawned.len(), 2, "both entities still spawn");
        assert_eq!(report.unresolved, 1);
        let transform = world
            .get::<Transform>(report.spawned[1])
            .expect("second entity keeps its transform");
        assert_eq!(transform.0.translation.x, 7.0);
    }

    #[test]
    fn instantiate_without_resolver_is_unchanged() {
        let scene = scene_with_one_sprite_entity();
        let mut world = World::new();

        let spawned = scene.instantiate(&mut world);

        assert_eq!(spawned.len(), 1);
        assert_eq!(
            world.get::<Name>(spawned[0]).map(|n| n.0.as_str()),
            Some("Coin"),
        );
        assert!(
            world
                .get::<engine_ecs::components::Sprite>(spawned[0])
                .is_none(),
            "NullResolver resolves nothing, exactly as before this trait existed",
        );
    }

    // --- T-02: capture of renderable references ---------------------

    use engine_ecs::components::{MeshSource, Sprite as EcsSprite, SpriteSource};

    const MESH_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
    const MATERIAL_ID: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
    const ATLAS_ID: &str = "16fd2706-8baf-433b-82eb-8c7fada847da";

    fn sprite_source() -> SpriteSource {
        SpriteSource {
            atlas: ATLAS_ID.into(),
            region: "coin_0".into(),
            size: [2.0, 3.0],
            color: [1.0, 0.5, 0.25, 1.0],
            z_order: 0.0,
        }
    }

    #[test]
    fn emitter_round_trips_through_the_world() {
        use crate::format::{AssetRef, AudioEmitterData};

        let emitter = AudioEmitterData {
            sound: AssetRef {
                id: "8a1d0f84-0a3a-4a1e-9d5b-2f3d4e5a6b7c".to_owned(),
            },
            autoplay: false,
            looping: true,
            gain: 0.4,
            radius: 12.5,
        };
        let scene = Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: vec![SceneEntity {
                name: Some("Waterfall".to_owned()),
                audio_emitter: Some(emitter.clone()),
                ..Default::default()
            }],
            nav_grid: None,
            lighting: None,
            environment: Default::default(),
        };

        let mut world = World::new();
        // No resolver, so no sound is decoded — the authored values must
        // survive anyway, or the editor deletes them on the next save.
        scene.instantiate(&mut world);
        let captured = Scene::from_world(&mut world);

        assert_eq!(captured.entities[0].audio_emitter, Some(emitter));
    }

    #[test]
    fn authored_emitter_gain_and_radius_reach_the_live_emitter() {
        use crate::{AssetRef, AudioEmitterData, SceneError, SceneResolver};
        use engine_ecs::AudioEmitter;

        struct SoundResolver;
        impl SceneResolver for SoundResolver {
            fn resolve_mesh(
                &mut self,
                _: &AssetRef,
                _: &AssetRef,
            ) -> Result<Option<engine_ecs::components::MeshRenderer>, SceneError> {
                Ok(None)
            }
            fn resolve_sprite_uv(
                &mut self,
                _: &AssetRef,
                _: &str,
            ) -> Result<Option<engine_renderer::UvRect>, SceneError> {
                Ok(None)
            }
            fn resolve_sound(
                &mut self,
                _: &AssetRef,
                _: bool,
            ) -> Result<Option<engine_audio::StaticSound>, SceneError> {
                let mut wav = b"RIFF".to_vec();
                wav.extend_from_slice(&38u32.to_le_bytes());
                wav.extend_from_slice(b"WAVEfmt ");
                wav.extend_from_slice(&16u32.to_le_bytes());
                wav.extend_from_slice(&1u16.to_le_bytes());
                wav.extend_from_slice(&1u16.to_le_bytes());
                wav.extend_from_slice(&44_100u32.to_le_bytes());
                wav.extend_from_slice(&88_200u32.to_le_bytes());
                wav.extend_from_slice(&2u16.to_le_bytes());
                wav.extend_from_slice(&16u16.to_le_bytes());
                wav.extend_from_slice(b"data");
                wav.extend_from_slice(&2u32.to_le_bytes());
                wav.extend_from_slice(&[0, 0]);
                Ok(engine_audio::StaticSound::from_bytes(wav, false).ok())
            }
        }

        let scene = Scene {
            entities: vec![SceneEntity {
                name: Some("Quiet bell".into()),
                audio_emitter: Some(AudioEmitterData {
                    sound: AssetRef {
                        id: "8a1d0f84-0a3a-4a1e-9d5b-2f3d4e5a6b7c".into(),
                    },
                    autoplay: false,
                    looping: false,
                    gain: 0.5,
                    radius: 12.5,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut world = World::new();
        let report = scene.instantiate_with_resolver(&mut world, &mut SoundResolver);
        assert_eq!(report.unresolved, 0);
        let emitter = world
            .get::<AudioEmitter>(report.spawned[0])
            .expect("live emitter");
        assert_eq!(emitter.gain, 0.5);
        assert_eq!(emitter.radius, 12.5);
    }

    #[test]
    fn nav_grid_round_trips() {
        use crate::format::NavGridData;

        let mut grid = NavGridData::new(4, 3, 2.0, [-4.0, -3.0]);
        grid.blocked[5] = true;
        grid.blocked[6] = true;

        let scene = Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: Vec::new(),
            nav_grid: Some(grid.clone()),
            lighting: None,
            environment: Default::default(),
        };

        let mut world = World::new();
        scene.instantiate(&mut world);
        let captured = Scene::from_world(&mut world);

        assert_eq!(
            captured.nav_grid,
            Some(grid),
            "a level's walkability is the level's, and has to survive a save"
        );
    }

    #[test]
    fn authored_lighting_survives_world_round_trip() {
        let lighting = crate::LightingData {
            sun_direction: [0.0, -1.0, 0.0],
            sun_color: [0.5, 0.6, 1.0],
            sun_intensity: 1.5,
            ambient_intensity: 0.2,
        };
        let scene = Scene {
            lighting: Some(lighting),
            ..Scene::default()
        };
        let mut world = World::new();
        scene.instantiate(&mut world);
        assert_eq!(Scene::from_world(&mut world).lighting, Some(lighting));
    }

    #[test]
    fn authored_water_survives_scene_save_reopen_and_instantiation() {
        use engine_ecs::components::WaterVolume;
        let surface = engine_physics::WaterSurface {
            center: glam::Vec3::new(2.0, 0.0, -3.0),
            half_extent: glam::Vec2::new(5.0, 7.0),
            base_height: 1.5,
            wave: engine_physics::WaterWave {
                amplitude: 0.2,
                ..Default::default()
            },
            current: glam::Vec3::new(0.5, 0.0, 0.0),
            density: 980.0,
            linear_drag: 0.8,
        };
        let mut world = World::new();
        world.spawn((Name::new("Pond"), WaterVolume { surface }));

        let text = Scene::from_world(&mut world).to_ron_string().unwrap();
        let reopened = Scene::from_ron_str(&text).unwrap();
        let mut loaded = World::new();
        let entities = reopened.instantiate(&mut loaded);
        assert_eq!(
            loaded.get::<WaterVolume>(entities[0]).unwrap().surface,
            surface
        );
        assert_eq!(Scene::from_world(&mut loaded).entities, reopened.entities);
    }

    #[test]
    fn point_light_survives_world_scene_world_round_trip() {
        let settings = crate::format::PointLightData {
            color: [0.2, 0.4, 0.8],
            intensity: 5.0,
            range: 7.0,
        };
        let mut world = World::new();
        world.spawn((
            Name::new("Lamp"),
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                2.0, 3.0, 4.0,
            ))),
            crate::format::ScenePointLight(settings),
        ));

        let scene = Scene::from_world(&mut world);
        assert_eq!(scene.entities[0].point_light, Some(settings));
        let mut reopened = World::new();
        let spawned = scene.instantiate(&mut reopened);
        assert_eq!(
            reopened
                .get::<crate::format::ScenePointLight>(spawned[0])
                .map(|light| light.0),
            Some(settings)
        );
        assert_eq!(Scene::from_world(&mut reopened), scene);
    }

    #[test]
    fn point_lights_use_parent_world_position_and_skip_disabled_entities() {
        let mut world = World::new();
        let parent = world
            .spawn(Transform::from(engine_utils::Transform::from_translation(
                Vec3::new(10.0, 0.0, 0.0),
            )))
            .id();
        world.spawn((
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                1.0, 2.0, 3.0,
            ))),
            bevy_ecs::hierarchy::ChildOf(parent),
            crate::format::ScenePointLight(crate::format::PointLightData {
                color: [0.2, 0.4, 0.8],
                intensity: 5.0,
                range: 7.0,
            }),
        ));
        world.spawn((
            Transform::default(),
            Disabled,
            crate::format::ScenePointLight(crate::format::PointLightData::default()),
        ));

        let lights = super::point_lights_from_world(&mut world);
        assert_eq!(lights.len(), 1);
        assert_eq!(lights[0].position, Vec3::new(11.0, 2.0, 3.0));
        assert_eq!(lights[0].color.to_array(), [0.2, 0.4, 0.8]);
        assert_eq!(lights[0].intensity, 5.0);
    }

    #[test]
    fn invalid_authored_water_is_rejected_before_instantiation() {
        let mut water = crate::WaterData::from(engine_physics::WaterSurface::default());
        water.half_extent[0] = 0.0;
        let scene = Scene {
            entities: vec![SceneEntity {
                name: Some("Bad pond".into()),
                water: Some(water),
                ..SceneEntity::default()
            }],
            ..Scene::default()
        };
        assert!(scene.validate().is_err());
        let text = scene.to_ron_string().unwrap();
        let parsed = Scene::from_ron_str(&text).unwrap();
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn scene_without_lighting_clears_previous_scene_override() {
        let mut world = World::new();
        world.insert_resource(crate::SceneLighting(crate::LightingData::default()));
        Scene::default().instantiate(&mut world);
        assert!(world.get_resource::<crate::SceneLighting>().is_none());
    }

    #[test]
    fn a_scene_without_a_nav_grid_does_not_invent_one() {
        let mut world = World::new();
        Scene::default().instantiate(&mut world);
        assert_eq!(Scene::from_world(&mut world).nav_grid, None);
    }

    #[test]
    fn a_collider_survives_a_round_trip_through_the_world() {
        // Without the `SceneCollider` carrier this silently deleted
        // every collider in a scene the moment the editor saved it —
        // 166 of them in the island's.
        use crate::format::{BodyKind, ColliderData, ColliderShape};

        let collider = ColliderData {
            shape: ColliderShape::Capsule {
                half_height: 0.9,
                radius: 0.35,
            },
            body: BodyKind::Dynamic,
        };
        let scene = Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: vec![SceneEntity {
                name: Some("Player".to_owned()),
                transform: Some(TransformData::from(engine_utils::Transform::IDENTITY)),
                collider: Some(collider.clone()),
                ..Default::default()
            }],
            nav_grid: None,
            lighting: None,
            environment: Default::default(),
        };

        let mut world = World::new();
        scene.instantiate(&mut world);
        let captured = Scene::from_world(&mut world);

        assert_eq!(
            captured.entities[0].collider,
            Some(collider),
            "a saved scene keeps the colliders the loaded one had"
        );
    }

    #[test]
    fn from_world_captures_mesh_renderer() {
        let mut world = World::new();
        world.spawn((Name::new("Crate"), MeshSource::new(MESH_ID, MATERIAL_ID)));

        let scene = Scene::from_world(&mut world);

        let mesh_renderer = scene.entities[0]
            .mesh_renderer
            .as_ref()
            .expect("mesh reference should be saved");
        assert_eq!(mesh_renderer.mesh.id, MESH_ID);
        assert_eq!(mesh_renderer.material.id, MATERIAL_ID);
    }

    #[test]
    fn from_world_captures_sprite() {
        let mut world = World::new();
        world.spawn((Name::new("Coin"), sprite_source()));

        let scene = Scene::from_world(&mut world);

        let sprite = scene.entities[0]
            .sprite
            .as_ref()
            .expect("sprite reference should be saved");
        assert_eq!(sprite.atlas.id, ATLAS_ID);
        assert_eq!(sprite.region, "coin_0");
        assert_eq!(sprite.size, [2.0, 3.0]);
    }

    #[test]
    fn sprite_animation_survives_world_scene_world_round_trip() {
        let mut world = World::new();
        let mut source = sprite_source();
        source.region = "grid:4,1:0,0".into();
        let animation = crate::SpriteAnimationData {
            frames: 4,
            fps: 8.0,
            looping: true,
        };
        world.spawn((
            Name::new("Coin"),
            source,
            crate::SceneSpriteAnimation::new(animation),
        ));

        let text = Scene::from_world(&mut world).to_ron_string().unwrap();
        let parsed = Scene::from_ron_str(&text).unwrap();
        let mut reopened = World::new();
        let entities = parsed.instantiate(&mut reopened);
        assert_eq!(
            reopened
                .get::<crate::SceneSpriteAnimation>(entities[0])
                .unwrap()
                .data,
            animation
        );
    }

    #[test]
    fn scene_round_trips_renderables() {
        let mut world = World::new();
        world.spawn((
            Name::new("Crate"),
            Transform::from(MathTransform::from_translation(Vec3::new(4.0, 0.0, 0.0))),
            MeshSource::new(MESH_ID, MATERIAL_ID),
        ));
        world.spawn((Name::new("Coin"), sprite_source()));

        // world -> Scene -> RON -> Scene -> world
        let text = Scene::from_world(&mut world)
            .to_ron_string()
            .expect("scene should serialize");
        let parsed = Scene::from_ron_str(&text).expect("scene should parse back");
        let mut reloaded = World::new();
        let report = parsed.instantiate_with_resolver(
            &mut reloaded,
            &mut crate::resolve::test_support::StubResolver::resolving_sprites(),
        );

        let named: std::collections::HashMap<String, bevy_ecs::prelude::Entity> = report
            .spawned
            .iter()
            .filter_map(|&entity| {
                reloaded
                    .get::<Name>(entity)
                    .map(|name| (name.0.clone(), entity))
            })
            .collect();

        let crate_entity = named["Crate"];
        let mesh = reloaded
            .get::<MeshSource>(crate_entity)
            .expect("mesh reference survives the round trip");
        assert_eq!(mesh.mesh, MESH_ID);
        assert_eq!(mesh.material, MATERIAL_ID);
        assert_eq!(
            reloaded
                .get::<Transform>(crate_entity)
                .map(|t| t.0.translation.x),
            Some(4.0),
        );

        let coin = named["Coin"];
        assert_eq!(
            reloaded.get::<SpriteSource>(coin).map(|s| s.region.clone()),
            Some("coin_0".to_string()),
        );
        assert!(
            reloaded.get::<EcsSprite>(coin).is_some(),
            "the resolver rebuilt the live sprite",
        );
    }

    #[test]
    fn material_override_survives_scene_reopen_and_unresolved_mesh() {
        let authored = crate::MaterialOverrideData {
            tint: [0.25, 0.5, 1.0],
            metallic: Some(0.8),
            roughness: Some(0.3),
        };
        let scene = Scene {
            entities: vec![SceneEntity {
                name: Some("Tinted crate".into()),
                mesh_renderer: Some(crate::MeshRendererData {
                    mesh: crate::AssetRef { id: MESH_ID.into() },
                    material: crate::AssetRef {
                        id: MATERIAL_ID.into(),
                    },
                    material_override: Some(authored.clone()),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let reopened = Scene::from_ron_str(&scene.to_ron_string().unwrap()).unwrap();
        let mut world = World::new();
        reopened.instantiate(&mut world);
        assert_eq!(
            Scene::from_world(&mut world).entities[0]
                .mesh_renderer
                .as_ref()
                .and_then(|mesh| mesh.material_override.as_ref()),
            Some(&authored),
        );
    }

    #[test]
    fn foliage_instances_survive_scene_reopen_without_the_model_file() {
        let authored = crate::FoliageData {
            mesh: crate::AssetRef { id: MESH_ID.into() },
            material: crate::AssetRef {
                id: MATERIAL_ID.into(),
            },
            instances: vec![crate::TransformData::from(
                engine_utils::Transform::from_translation(glam::Vec3::new(3.0, 1.0, -2.0)),
            )],
        };
        let scene = Scene {
            entities: vec![SceneEntity {
                name: Some("Grass patch".into()),
                foliage: Some(authored.clone()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let reopened = Scene::from_ron_str(&scene.to_ron_string().unwrap()).unwrap();
        let mut world = World::new();
        reopened.instantiate(&mut world);
        assert_eq!(
            Scene::from_world(&mut world).entities[0].foliage,
            Some(authored)
        );
    }

    #[test]
    fn unresolved_reference_survives_a_save_cycle() {
        // The data-loss guard: open a scene whose asset file is gone,
        // save it again, and the reference must still be in the file.
        let original = Scene {
            entities: vec![SceneEntity {
                name: Some("Crate".into()),
                mesh_renderer: Some(crate::format::MeshRendererData {
                    mesh: crate::format::AssetRef { id: MESH_ID.into() },
                    material: crate::format::AssetRef {
                        id: MATERIAL_ID.into(),
                    },
                    material_override: None,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut world = World::new();
        let report = original.instantiate_with_resolver(
            &mut world,
            &mut crate::resolve::test_support::StubResolver::failing(),
        );
        assert_eq!(
            report.unresolved, 1,
            "the asset genuinely failed to resolve"
        );

        let resaved = Scene::from_world(&mut world);

        let mesh_renderer = resaved.entities[0]
            .mesh_renderer
            .as_ref()
            .expect("a reference must never be deleted by re-saving an unresolved scene");
        assert_eq!(mesh_renderer.mesh.id, MESH_ID);
        assert_eq!(mesh_renderer.material.id, MATERIAL_ID);
    }

    #[test]
    fn live_sprite_edits_win_over_stored_values() {
        let mut world = World::new();
        let mut edited = EcsSprite::new(
            glam::Vec2::new(8.0, 8.0),
            engine_renderer::UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        );
        edited.color = [0.0, 0.0, 1.0, 1.0];
        world.spawn((Name::new("Coin"), sprite_source(), edited));

        let scene = Scene::from_world(&mut world);

        let sprite = scene.entities[0].sprite.as_ref().expect("sprite saved");
        assert_eq!(sprite.size, [8.0, 8.0], "the Inspector edit must be saved");
        assert_eq!(sprite.color, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn old_scene_ron_without_renderables_still_parses() {
        // A pre-T-02 file: no `mesh_renderer`, no `sprite` keys at all.
        let text = r#"(
            version: 2,
            entities: [
                (
                    name: Some("Crate"),
                    transform: Some((
                        translation: (1.0, 2.0, 3.0),
                        rotation: (0.0, 0.0, 0.0, 1.0),
                        scale: (1.0, 1.0, 1.0),
                    )),
                ),
            ],
        )"#;

        let scene = Scene::from_ron_str(text).expect("old scene files must keep parsing");

        assert_eq!(scene.entities.len(), 1);
        assert!(scene.entities[0].mesh_renderer.is_none());
        assert!(scene.entities[0].sprite.is_none());
    }

    #[test]
    fn validate_rejects_an_empty_asset_ref() {
        let scene = Scene {
            entities: vec![SceneEntity {
                name: Some("Crate".into()),
                mesh_renderer: Some(crate::format::MeshRendererData {
                    mesh: crate::format::AssetRef { id: String::new() },
                    material: crate::format::AssetRef {
                        id: MATERIAL_ID.into(),
                    },
                    material_override: None,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert!(scene.validate().is_err());
    }
}
