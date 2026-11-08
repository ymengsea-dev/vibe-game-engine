//! Copying objects: Duplicate and copy/paste in the Hierarchy.
//!
//! A copy goes through the scene format, the same round trip undo uses, so it
//! carries everything an object saves: mesh, collider, light, script and so
//! on. Renderers come back through the usual [`crate::resolve_pending`] pass.

use std::collections::HashMap;

use engine_ecs::prelude::{ChildOf, Entity, World};
use engine_scene::{Scene, SceneEntity, SceneObjectId};
use engine_utils::Transform as MathTransform;

use crate::hierarchy::{
    children_of, parent_of, place_among_siblings, set_world_transform, top_objects, world_transform,
};

/// Added to the name of a copied top object.
const COPY_SUFFIX: &str = " Copy";

/// Objects held for pasting. Independent of the world they came from, so
/// deleting the originals does not affect it.
#[derive(Debug, Clone)]
pub struct ObjectClipboard {
    /// The objects and everything under them; `parent` indexes this list.
    entities: Vec<SceneEntity>,
    /// Each top object's index in `entities` and its world transform.
    tops: Vec<(usize, MathTransform)>,
}

/// Captures `entities` and their children. An object whose parent is also
/// listed is taken with that parent. Returns `None` when nothing can be
/// copied; objects with no name are not part of a scene.
pub fn copy_objects(world: &mut World, entities: &[Entity]) -> Option<ObjectClipboard> {
    let (scene, order) = Scene::from_world_with_entities(world);
    let index_of: HashMap<Entity, usize> = order
        .iter()
        .enumerate()
        .map(|(index, &entity)| (entity, index))
        .collect();

    let mut clipboard = ObjectClipboard {
        entities: Vec::new(),
        tops: Vec::new(),
    };
    for top in top_objects(world, entities) {
        if let Some(index) = copy_subtree(world, &scene, &index_of, top, None, &mut clipboard) {
            clipboard.tops.push((index, world_transform(world, top)));
        }
    }
    (!clipboard.tops.is_empty()).then_some(clipboard)
}

fn copy_subtree(
    world: &World,
    scene: &Scene,
    index_of: &HashMap<Entity, usize>,
    entity: Entity,
    parent: Option<usize>,
    clipboard: &mut ObjectClipboard,
) -> Option<usize> {
    let mut copy = scene.entities.get(*index_of.get(&entity)?)?.clone();
    copy.parent = parent;
    // A scene has one active camera, and it stays the original.
    if let Some(camera) = &mut copy.camera {
        camera.active = false;
    }
    let index = clipboard.entities.len();
    clipboard.entities.push(copy);
    for child in children_of(world, entity) {
        copy_subtree(world, scene, index_of, child, Some(index), clipboard);
    }
    Some(index)
}

/// Spawns a copy of the clipboard's objects under `parent` (the scene root
/// when `None`), directly after `after` if it is one of `parent`'s children
/// and otherwise last. Each stays where its original was in the world. Every
/// copy gets a new id, and references between copied objects are pointed at
/// the copies. Returns the top objects.
pub fn paste_objects(
    world: &mut World,
    clipboard: &ObjectClipboard,
    parent: Option<Entity>,
    after: Option<Entity>,
) -> Vec<Entity> {
    let parent = parent.filter(|&parent| world.get_entity(parent).is_ok());
    let mut entities = clipboard.entities.clone();
    let mut new_ids = HashMap::new();
    for entity in &mut entities {
        let new_id = uuid::Uuid::new_v4().to_string();
        if let Some(old_id) = entity.object_id.replace(new_id.clone()) {
            new_ids.insert(old_id, new_id);
        }
    }
    for &(index, _) in &clipboard.tops {
        if let Some(name) = entities.get_mut(index).and_then(|top| top.name.as_mut()) {
            name.push_str(COPY_SUFFIX);
        }
    }

    let spawned = Scene::spawn_entities(&entities, world);
    remap_object_references(world, &spawned, &new_ids);

    let tops: Vec<Entity> = clipboard
        .tops
        .iter()
        .filter_map(|&(index, _)| spawned.get(index).copied())
        .collect();
    if let Some(parent) = parent {
        for &top in &tops {
            world.entity_mut(top).insert(ChildOf(parent));
        }
    }
    place_among_siblings(world, &tops, parent, after);
    for (&top, &(_, transform)) in tops.iter().zip(&clipboard.tops) {
        if world
            .get::<engine_ecs::components::Transform>(top)
            .is_some()
        {
            set_world_transform(world, top, transform);
        }
    }
    tops
}

/// Copies each of `entities` directly after its original, under the same
/// parent. Returns the copies' top objects.
pub fn duplicate_objects(world: &mut World, entities: &[Entity]) -> Vec<Entity> {
    let mut copies = Vec::new();
    for source in top_objects(world, entities) {
        if let Some(clipboard) = copy_objects(world, &[source]) {
            let parent = parent_of(world, source);
            copies.extend(paste_objects(world, &clipboard, parent, Some(source)));
        }
    }
    copies
}

/// Points everything in `copies` that names an object by id at its copy,
/// where `new_ids` has one. Other references are left as they are.
fn remap_object_references(
    world: &mut World,
    copies: &[Entity],
    new_ids: &HashMap<String, String>,
) {
    for &copied in copies {
        if let Some(mut follow) = world.get_mut::<engine_ecs::components::CameraFollow>(copied)
            && let Some(new_id) = new_ids.get(&follow.target)
        {
            follow.target = new_id.clone();
        }
        if let Some(mut rules) = world.get_mut::<engine_scene::SceneRules>(copied) {
            for rule in &mut rules.0 {
                let target = match &mut rule.action {
                    engine_scene::RuleAction::SetUiText { target, .. }
                    | engine_scene::RuleAction::PlayAudio { target }
                    | engine_scene::RuleAction::SetVisible { target, .. }
                    | engine_scene::RuleAction::SetEnabled { target, .. }
                    | engine_scene::RuleAction::Destroy { target } => Some(target),
                    engine_scene::RuleAction::EmitEvent { .. }
                    | engine_scene::RuleAction::SetVariable { .. }
                    | engine_scene::RuleAction::AddToVariable { .. }
                    | engine_scene::RuleAction::ChangeScene { .. }
                    | engine_scene::RuleAction::SpawnPrefab { .. }
                    | engine_scene::RuleAction::SetGameState { .. } => None,
                };
                if let Some(target) = target
                    && let Some(new_id) = new_ids.get(target)
                {
                    *target = new_id.clone();
                }
                let extra = match (&mut rule.action, &mut rule.trigger) {
                    (
                        engine_scene::RuleAction::SpawnPrefab {
                            at: engine_scene::SpawnAt::Object(id),
                            ..
                        },
                        _,
                    ) => Some(id),
                    _ => None,
                };
                if let Some(id) = extra
                    && let Some(new_id) = new_ids.get(id)
                {
                    *id = new_id.clone();
                }
                if let engine_scene::RuleTrigger::CollisionEnter { with: Some(id) }
                | engine_scene::RuleTrigger::CollisionExit { with: Some(id) }
                | engine_scene::RuleTrigger::TriggerEnter { with: Some(id) }
                | engine_scene::RuleTrigger::TriggerExit { with: Some(id) } = &mut rule.trigger
                    && let Some(new_id) = new_ids.get(id)
                {
                    *id = new_id.clone();
                }
            }
        }
    }
}

/// The id of `entity`, giving it one if it has none yet.
pub(crate) fn ensure_object_id(world: &mut World, entity: Entity) -> Option<String> {
    if let Some(id) = world.get::<SceneObjectId>(entity) {
        return Some(id.0.clone());
    }
    let id = uuid::Uuid::new_v4().to_string();
    world
        .get_entity_mut(entity)
        .ok()?
        .insert(SceneObjectId(id.clone()));
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hierarchy::{reparent, spawn_entity};
    use engine_ecs::components::{Lock, Name, Static, WaterVolume};
    use engine_scene::{ColliderData, PointLightData, SceneCollider, ScenePointLight};
    use glam::Vec3;

    fn saved(world: &mut World, entity: Entity) -> SceneEntity {
        let (scene, order) = Scene::from_world_with_entities(world);
        let index = order.iter().position(|&live| live == entity).unwrap();
        scene.entities[index].clone()
    }

    fn name_of(world: &World, entity: Entity) -> String {
        world.get::<Name>(entity).unwrap().0.clone()
    }

    #[test]
    fn a_duplicate_keeps_everything_the_object_saves() {
        let mut world = World::new();
        let source = spawn_entity(&mut world, "Rock", Vec3::new(1.0, 2.0, 3.0));
        world.entity_mut(source).insert((
            engine_ecs::components::MeshSource::new("mesh-id", "material-id"),
            SceneCollider(ColliderData::new(
                engine_scene::ColliderShape::Cuboid {
                    half_extents: [1.0, 1.0, 1.0],
                },
                engine_scene::BodyKind::Fixed,
            )),
            ScenePointLight(PointLightData::default()),
            WaterVolume {
                surface: Default::default(),
            },
            engine_ecs::components::Script::new("scripts/rock.lua"),
            Static,
            Lock,
        ));

        let copies = duplicate_objects(&mut world, &[source]);
        assert_eq!(copies.len(), 1);

        let mut expected = saved(&mut world, source);
        let mut actual = saved(&mut world, copies[0]);
        assert_ne!(actual.object_id, expected.object_id);
        assert_eq!(actual.name.as_deref(), Some("Rock Copy"));
        for entity in [&mut expected, &mut actual] {
            entity.object_id = None;
            entity.name = None;
            entity.sibling_order = None;
        }
        assert_eq!(actual, expected);
        assert!(actual.mesh_renderer.is_some(), "the mesh came along");
    }

    #[test]
    fn a_duplicate_stays_under_its_parent_right_after_the_original() {
        let mut world = World::new();
        let parent = spawn_entity(&mut world, "Parent", Vec3::new(5.0, 0.0, 0.0));
        let first = spawn_entity(&mut world, "First", Vec3::new(6.0, 0.0, 0.0));
        let last = spawn_entity(&mut world, "Last", Vec3::ZERO);
        reparent(&mut world, first, Some(parent));
        reparent(&mut world, last, Some(parent));
        place_among_siblings(&mut world, &[first, last], Some(parent), None);

        let copies = duplicate_objects(&mut world, &[first]);

        assert_eq!(parent_of(&world, copies[0]), Some(parent));
        assert_eq!(children_of(&world, parent), vec![first, copies[0], last]);
        assert!(
            world_transform(&world, copies[0])
                .translation
                .abs_diff_eq(Vec3::new(6.0, 0.0, 0.0), 1e-5),
            "the copy does not jump"
        );
    }

    #[test]
    fn a_parent_and_its_child_copy_the_subtree_once() {
        let mut world = World::new();
        let parent = spawn_entity(&mut world, "Parent", Vec3::ZERO);
        let child = spawn_entity(&mut world, "Child", Vec3::ZERO);
        reparent(&mut world, child, Some(parent));

        let copies = duplicate_objects(&mut world, &[child, parent]);

        assert_eq!(copies.len(), 1);
        assert_eq!(name_of(&world, copies[0]), "Parent Copy");
        let inner = children_of(&world, copies[0]);
        assert_eq!(inner.len(), 1);
        assert_eq!(
            name_of(&world, inner[0]),
            "Child",
            "only the top is renamed"
        );
        assert_eq!(children_of(&world, parent), vec![child]);
    }

    #[test]
    fn references_follow_the_copies_only_inside_the_copied_set() {
        use engine_scene::{RuleAction, RuleData, RuleTrigger, SceneRules};
        let mut world = World::new();
        let outside = spawn_entity(&mut world, "Outside", Vec3::ZERO);
        let outside_id = ensure_object_id(&mut world, outside).unwrap();
        let parent = spawn_entity(&mut world, "Parent", Vec3::ZERO);
        let child = spawn_entity(&mut world, "Child", Vec3::ZERO);
        reparent(&mut world, child, Some(parent));
        let child_id = ensure_object_id(&mut world, child).unwrap();
        let rule = |target: &str| {
            RuleData::new(
                RuleTrigger::UiClicked,
                RuleAction::Destroy {
                    target: target.to_owned(),
                },
            )
        };
        world
            .entity_mut(parent)
            .insert(SceneRules(vec![rule(&child_id), rule(&outside_id)]));

        let copies = duplicate_objects(&mut world, &[parent]);

        let inner = children_of(&world, copies[0])[0];
        let inner_id = world.get::<SceneObjectId>(inner).unwrap().0.clone();
        assert_ne!(inner_id, child_id);
        let rules = &world.get::<SceneRules>(copies[0]).unwrap().0;
        let target = |rule: &RuleData| match &rule.action {
            RuleAction::Destroy { target } => target.clone(),
            other => panic!("unexpected action {other:?}"),
        };
        assert_eq!(target(&rules[0]), inner_id);
        assert_eq!(target(&rules[1]), outside_id);
    }

    #[test]
    fn the_active_camera_stays_the_original() {
        let mut world = World::new();
        let camera = spawn_entity(&mut world, "Camera", Vec3::ZERO);
        world.entity_mut(camera).insert((
            engine_ecs::components::Camera::from(engine_renderer::Camera::new(
                Vec3::ZERO,
                Vec3::NEG_Z,
                1.0,
            )),
            engine_ecs::components::ActiveCamera,
        ));

        let copies = duplicate_objects(&mut world, &[camera]);

        assert!(
            world
                .get::<engine_ecs::components::Camera>(copies[0])
                .is_some()
        );
        assert!(
            world
                .get::<engine_ecs::components::ActiveCamera>(copies[0])
                .is_none()
        );
    }

    #[test]
    fn pasting_leaves_scene_wide_settings_alone() {
        let mut world = World::new();
        world.insert_resource(engine_scene::SceneLighting(
            engine_scene::LightingData::default(),
        ));
        let source = spawn_entity(&mut world, "Rock", Vec3::ZERO);

        let clipboard = copy_objects(&mut world, &[source]).unwrap();
        world.remove_resource::<engine_scene::SceneLighting>();
        world.insert_resource(engine_scene::SceneLighting(
            engine_scene::LightingData::default(),
        ));
        paste_objects(&mut world, &clipboard, None, None);

        assert!(
            world
                .get_resource::<engine_scene::SceneLighting>()
                .is_some()
        );
    }

    #[test]
    fn pasting_elsewhere_keeps_world_position_and_gives_new_ids() {
        let mut world = World::new();
        let source = spawn_entity(&mut world, "Rock", Vec3::new(1.0, 0.0, 0.0));
        let elsewhere = spawn_entity(&mut world, "Elsewhere", Vec3::new(10.0, 0.0, 0.0));

        let clipboard = copy_objects(&mut world, &[source]).unwrap();
        despawn_source(&mut world, source);
        let first = paste_objects(&mut world, &clipboard, Some(elsewhere), None)[0];
        let second = paste_objects(&mut world, &clipboard, None, None)[0];

        assert_eq!(parent_of(&world, first), Some(elsewhere));
        assert!(
            world_transform(&world, first)
                .translation
                .abs_diff_eq(Vec3::new(1.0, 0.0, 0.0), 1e-5)
        );
        assert_ne!(
            world.get::<SceneObjectId>(first).unwrap().0,
            world.get::<SceneObjectId>(second).unwrap().0,
        );
    }

    fn despawn_source(world: &mut World, source: Entity) {
        crate::hierarchy::despawn_entity(world, source);
    }

    #[test]
    fn an_object_with_no_name_cannot_be_copied() {
        let mut world = World::new();
        let unnamed = world.spawn_empty().id();
        assert!(copy_objects(&mut world, &[unnamed]).is_none());
        assert!(duplicate_objects(&mut world, &[unnamed]).is_empty());
    }
}
