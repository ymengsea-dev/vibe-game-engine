//! Hierarchy panel: lists the ECS world's entities as a parent/child
//! tree and tracks which one is selected.
//!
//! Entity-listing logic ([`roots`], [`children_of`], [`display_name`])
//! is split out from the egui drawing ([`show`]) so it's unit-testable
//! without a live UI — the same separation `engine_renderer` uses
//! between pure config-building functions and the GPU calls that need a
//! live run to verify.
//!
//! Lists whatever `World` it's given — the same one
//! [`crate::EditorState::entity_transforms`] reads from to drive
//! [`crate::Viewport`], so an entity spawned/deleted here (or renamed/
//! moved via [`crate::inspector`]/the Scene View's gizmo) shows up there
//! too. [`spawn_entity`]/[`despawn_entity`] back the panel's "+ Cube"/
//! "Delete" buttons — the only way to actually create or remove an
//! entity in the editor so far.

use engine_ecs::components::{Name, Transform as TransformComponent};
use engine_ecs::prelude::{ChildOf, Children, Entity, With, Without, World};
use glam::Vec3;

/// This world's root entities: named (see below), and not parented to
/// anything else (no `ChildOf`). Stable (sorted by [`Entity`]) order so
/// the tree doesn't reshuffle from one frame to the next.
///
/// Requiring [`Name`] at the root is deliberate, not just incidental:
/// `bevy_ecs` reserves a handful of entity indices of its own in every
/// `World` for internal bookkeeping (observers, lifecycle hooks — see
/// `Ecs::new`'s tests for the same quirk), and those have no components
/// a game or the editor ever sets. Nothing else distinguishes them from
/// a legitimate empty root, so "must have a name to show up as a root"
/// is what keeps them out of the tree. Unnamed *children* still display
/// fine (see [`display_name`]'s fallback) — this restriction is root-only.
pub fn roots(world: &mut World) -> Vec<Entity> {
    let mut query = world.query_filtered::<Entity, (With<Name>, Without<ChildOf>)>();
    let mut entities: Vec<Entity> = query.iter(world).collect();
    entities.sort();
    entities
}

/// `entity`'s direct children, in a stable (sorted by [`Entity`]) order —
/// empty if it has none (either no [`Children`] component, or one with
/// no entries).
pub fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
    let mut children: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();
    children.sort();
    children
}

/// The label to show for `entity`: its [`Name`] component's text if it
/// has one, otherwise a fallback built from its raw [`Entity`] id.
pub fn display_name(world: &World, entity: Entity) -> String {
    world
        .get::<Name>(entity)
        .map(|name| name.0.clone())
        .unwrap_or_else(|| format!("Entity {entity:?}"))
}

/// Spawns a new entity named `name` at world-space `position` (a
/// [`Name`] plus a [`TransformComponent`], nothing else) and returns it
/// — what the hierarchy panel's "+ Cube" button does, so there's
/// something new to select, move (via the Scene View's gizmo), and see
/// (via [`crate::EditorState::entity_transforms`]/[`crate::Viewport`]).
pub fn spawn_entity(world: &mut World, name: impl Into<String>, position: Vec3) -> Entity {
    world
        .spawn((
            Name::new(name),
            TransformComponent::from(engine_utils::Transform::from_translation(position)),
        ))
        .id()
}

/// Despawns `entity` — a harmless no-op if it doesn't exist (e.g. it was
/// already deleted, or the caller's `selected` was stale).
pub fn despawn_entity(world: &mut World, entity: Entity) {
    world.despawn(entity);
}

/// Draws the hierarchy panel into `ui`: a toolbar ("+ Cube" to spawn a
/// new entity, "Delete" for whatever's selected) above the tree itself,
/// updating `*selected` when an entity is clicked, spawned, or deleted.
///
/// Entities with children are drawn as a [`egui::CollapsingHeader`]
/// (click to expand/collapse); entities without children are drawn as a
/// selectable row. Parent rows aren't selectable yet in this first
/// cut — only leaves are — since egui's `CollapsingHeader` doesn't
/// separate "toggle expand" from "select" clicks on the same header
/// without more custom widget work than this iteration's scope covers.
pub fn show(ui: &mut egui::Ui, world: &mut World, selected: &mut Option<Entity>) {
    ui.horizontal(|ui| {
        if ui.button("+ Cube").clicked() {
            let offset = roots(world).len() as f32 * 1.5;
            let entity = spawn_entity(world, "Cube", Vec3::new(offset, 0.0, 0.0));
            *selected = Some(entity);
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Delete"))
            .clicked()
            && let Some(entity) = selected.take()
        {
            despawn_entity(world, entity);
        }
    });
    ui.separator();

    for entity in roots(world) {
        show_entity(ui, world, entity, selected);
    }
}

fn show_entity(
    ui: &mut egui::Ui,
    world: &mut World,
    entity: Entity,
    selected: &mut Option<Entity>,
) {
    let label = display_name(world, entity);
    let children = children_of(world, entity);

    if children.is_empty() {
        let is_selected = *selected == Some(entity);
        if ui.selectable_label(is_selected, label).clicked() {
            *selected = Some(entity);
        }
    } else {
        egui::CollapsingHeader::new(label)
            .id_salt(entity)
            .default_open(true)
            .show(ui, |ui| {
                for child in children {
                    show_entity(ui, world, child, selected);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::prelude::ChildOf;

    #[test]
    fn spawn_entity_sets_name_and_transform() {
        let mut world = World::new();
        let entity = spawn_entity(&mut world, "Cube", Vec3::new(1.0, 2.0, 3.0));

        assert_eq!(display_name(&world, entity), "Cube");
        let transform = world.get::<TransformComponent>(entity).unwrap();
        assert_eq!(transform.0.translation, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn spawn_entity_becomes_a_root() {
        let mut world = World::new();
        let entity = spawn_entity(&mut world, "Cube", Vec3::ZERO);
        assert_eq!(roots(&mut world), vec![entity]);
    }

    #[test]
    fn despawn_entity_removes_it() {
        let mut world = World::new();
        let entity = spawn_entity(&mut world, "Cube", Vec3::ZERO);
        despawn_entity(&mut world, entity);
        assert!(roots(&mut world).is_empty());
    }

    #[test]
    fn despawn_entity_on_an_already_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let entity = spawn_entity(&mut world, "Cube", Vec3::ZERO);
        despawn_entity(&mut world, entity);
        despawn_entity(&mut world, entity); // must not panic
    }

    #[test]
    fn roots_excludes_children() {
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        let _child = world.spawn((Name::new("Child"), ChildOf(parent))).id();

        assert_eq!(roots(&mut world), vec![parent]);
    }

    #[test]
    fn roots_are_sorted_and_include_multiple_independent_entities() {
        let mut world = World::new();
        let a = world.spawn(Name::new("A")).id();
        let b = world.spawn(Name::new("B")).id();

        let mut result = roots(&mut world);
        result.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn children_of_returns_empty_for_a_leaf() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("Leaf")).id();
        assert!(children_of(&world, entity).is_empty());
    }

    #[test]
    fn children_of_returns_direct_children_sorted() {
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        let child_a = world.spawn(ChildOf(parent)).id();
        let child_b = world.spawn(ChildOf(parent)).id();

        let mut expected = vec![child_a, child_b];
        expected.sort();
        assert_eq!(children_of(&world, parent), expected);
    }

    #[test]
    fn display_name_uses_the_name_component_when_present() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("Camera")).id();
        assert_eq!(display_name(&world, entity), "Camera");
    }

    #[test]
    fn display_name_falls_back_to_entity_id_when_unnamed() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        assert_eq!(display_name(&world, entity), format!("Entity {entity:?}"));
    }
}
