//! Inspector panel: edits the selected entity's components.
//!
//! Shows [`Name`] (an editable text field) and [`Transform`] (translation
//! and scale as raw XYZ, rotation as XYZ Euler degrees — easier to edit
//! by hand than a raw quaternion) for whichever entity [`crate::hierarchy`]
//! has selected. Component get/set logic ([`name_of`], [`set_name`],
//! [`transform_of`], [`set_transform`]) is split out from the egui
//! drawing ([`show`]) so it's unit-testable without a live UI — the same
//! separation [`crate::hierarchy`] uses.
//!
//! Only `Name`/`Transform` are editable so far — the placeholder scene
//! [`crate::Viewport`] renders doesn't (yet) share entities with this
//! panel's `World` at all (see `crate::hierarchy`'s module docs), and
//! other component types (`MeshRenderer`, `RigidBody`, ...) get their own
//! inspector UI in later iterations as the editor needs them.

use engine_ecs::components::{Name, Transform};
use engine_ecs::prelude::{Entity, World};
use engine_utils::Transform as MathTransform;
use glam::{EulerRot, Quat, Vec3};

/// `entity`'s current name, or `None` if it has no [`Name`] component.
pub fn name_of(world: &World, entity: Entity) -> Option<String> {
    world.get::<Name>(entity).map(|name| name.0.clone())
}

/// Sets `entity`'s [`Name`] to `name`, adding the component if it didn't
/// already have one.
///
/// No-op (rather than panicking) if `entity` no longer exists — the
/// inspector can be showing a selection that was despawned since.
pub fn set_name(world: &mut World, entity: Entity, name: impl Into<String>) {
    let name = name.into();
    if let Some(mut existing) = world.get_mut::<Name>(entity) {
        existing.0 = name;
    } else if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(Name::new(name));
    }
}

/// `entity`'s current [`Transform`], or `None` if it has no `Transform`
/// component.
pub fn transform_of(world: &World, entity: Entity) -> Option<MathTransform> {
    world.get::<Transform>(entity).map(|transform| transform.0)
}

/// Sets `entity`'s [`Transform`] to `transform`, adding the component if
/// it didn't already have one. Same despawned-entity no-op as
/// [`set_name`].
pub fn set_transform(world: &mut World, entity: Entity, transform: MathTransform) {
    if let Some(mut existing) = world.get_mut::<Transform>(entity) {
        existing.0 = transform;
    } else if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(Transform::from(transform));
    }
}

/// Draws the inspector for `selected`, or a placeholder message if
/// nothing (or a since-despawned entity) is selected.
pub fn show(ui: &mut egui::Ui, world: &mut World, selected: Option<Entity>) {
    let Some(entity) = selected else {
        ui.label("No entity selected.");
        return;
    };
    if world.get_entity(entity).is_err() {
        ui.label("Selected entity no longer exists.");
        return;
    }

    show_name(ui, world, entity);
    ui.separator();
    show_transform(ui, world, entity);
}

fn show_name(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    let mut name = name_of(world, entity).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label("Name");
        if ui.text_edit_singleline(&mut name).changed() {
            set_name(world, entity, name);
        }
    });
}

fn show_transform(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    ui.label("Transform");
    let Some(transform) = transform_of(world, entity) else {
        ui.label("(no Transform component)");
        return;
    };

    let mut translation = transform.translation;
    let (rx, ry, rz) = transform.rotation.to_euler(EulerRot::XYZ);
    let mut euler_degrees = Vec3::new(rx.to_degrees(), ry.to_degrees(), rz.to_degrees());
    let mut scale = transform.scale;
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label("Translation");
        changed |= drag_vec3(ui, &mut translation, 0.05);
    });
    ui.horizontal(|ui| {
        ui.label("Rotation °");
        changed |= drag_vec3(ui, &mut euler_degrees, 1.0);
    });
    ui.horizontal(|ui| {
        ui.label("Scale");
        changed |= drag_vec3(ui, &mut scale, 0.05);
    });

    if changed {
        let rotation = Quat::from_euler(
            EulerRot::XYZ,
            euler_degrees.x.to_radians(),
            euler_degrees.y.to_radians(),
            euler_degrees.z.to_radians(),
        );
        set_transform(
            world,
            entity,
            MathTransform {
                translation,
                rotation,
                scale,
            },
        );
    }
}

/// Draws three `DragValue`s bound to `value`'s components, returning
/// whether any of them changed this frame.
fn drag_vec3(ui: &mut egui::Ui, value: &mut Vec3, speed: f32) -> bool {
    let mut changed = false;
    changed |= ui
        .add(egui::DragValue::new(&mut value.x).speed(speed))
        .changed();
    changed |= ui
        .add(egui::DragValue::new(&mut value.y).speed(speed))
        .changed();
    changed |= ui
        .add(egui::DragValue::new(&mut value.z).speed(speed))
        .changed();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_of_returns_none_when_absent() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        assert_eq!(name_of(&world, entity), None);
    }

    #[test]
    fn set_name_adds_the_component_when_absent() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        set_name(&mut world, entity, "Camera");
        assert_eq!(name_of(&world, entity), Some("Camera".to_string()));
    }

    #[test]
    fn set_name_overwrites_an_existing_name() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("Old")).id();
        set_name(&mut world, entity, "New");
        assert_eq!(name_of(&world, entity), Some("New".to_string()));
    }

    #[test]
    fn set_name_on_a_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        world.despawn(entity);
        set_name(&mut world, entity, "Ghost"); // must not panic
    }

    #[test]
    fn transform_of_returns_none_when_absent() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        assert_eq!(transform_of(&world, entity), None);
    }

    #[test]
    fn set_transform_adds_the_component_when_absent() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let transform = MathTransform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        set_transform(&mut world, entity, transform);
        assert_eq!(transform_of(&world, entity), Some(transform));
    }

    #[test]
    fn set_transform_overwrites_an_existing_transform() {
        let mut world = World::new();
        let entity = world.spawn(Transform::from(MathTransform::IDENTITY)).id();
        let transform = MathTransform::from_scale(Vec3::splat(2.0));
        set_transform(&mut world, entity, transform);
        assert_eq!(transform_of(&world, entity), Some(transform));
    }

    #[test]
    fn set_transform_on_a_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        world.despawn(entity);
        set_transform(&mut world, entity, MathTransform::IDENTITY); // must not panic
    }
}
