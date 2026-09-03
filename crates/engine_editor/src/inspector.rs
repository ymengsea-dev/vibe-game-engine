//! Inspector panel: an entity-identity header plus one editable section
//! per component the selected entity carries.
//!
//! Component types the panel knows how to show, add, and remove live in
//! a `const` table of function pointers (`REGISTRY`), so a new type is
//! one entry. The identity header shows the entity id, an editable
//! [`Name`], and the [`Static`]/[`Disabled`] markers. Get/set helpers
//! ([`name_of`], [`set_name`], [`transform_of`], [`set_transform`]) stay
//! split out from the egui drawing so they're unit-testable without a
//! live UI — the same separation [`crate::hierarchy`] uses; the Scene
//! View gizmo also calls [`transform_of`]/[`set_transform`] directly.

use engine_ecs::components::{
    AssetSource, Camera, Disabled, Lock, Name, Sprite, Static, Transform,
};
use engine_ecs::prelude::{Component, Entity, World};
use engine_utils::Transform as MathTransform;
use glam::{Vec2, Vec3};

use crate::inspect::{InspectField, Inspectable};

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

/// One component type the Inspector can show, add, and remove. A `const`
/// table entry — see `REGISTRY`.
struct ComponentUi {
    /// Section heading and Add-Component menu label.
    name: &'static str,
    /// The Rust type's identifier (may differ from `name`, e.g.
    /// `"AssetSource"` vs `"Asset Source"`) — the query the "Open
    /// Script" button hands to rust-analyzer.
    type_name: &'static str,
    /// Whether `entity` currently has this component.
    present: fn(&World, Entity) -> bool,
    /// Insert a sensible default instance on `entity`.
    insert_default: fn(&mut World, Entity),
    /// Remove it from `entity`.
    remove: fn(&mut World, Entity),
    /// Draw its editor; return whether the user changed anything.
    inspect: fn(&mut World, Entity, &mut egui::Ui) -> bool,
    /// Copy this component's value from `from` onto `to` (if `to`
    /// already has it) — the batch-edit "apply to all selected" path.
    copy_to: fn(&mut World, Entity, Entity),
}

/// Generic "copy `T` from `from` to `to`, only if `to` already has it".
/// Despawn-safe; used as each [`ComponentUi::copy_to`].
fn copy_component<T: Component + Clone>(world: &mut World, from: Entity, to: Entity) {
    if world.get::<T>(to).is_none() {
        return;
    }
    if let Some(value) = world.get::<T>(from).cloned()
        && let Ok(mut entity_mut) = world.get_entity_mut(to)
    {
        entity_mut.insert(value);
    }
}

/// Generic "does `entity` have `T`" — monomorphises to one fn pointer per
/// component type.
fn has<T: Component>(world: &World, entity: Entity) -> bool {
    world.get::<T>(entity).is_some()
}

/// Generic "remove `T` from `entity`", despawn-safe.
fn strip<T: Component>(world: &mut World, entity: Entity) {
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.remove::<T>();
    }
}

/// The component types the Inspector knows. Adding a type here is all it
/// takes for the section, the ✕ remove button, and the Add-Component
/// menu to pick it up.
const REGISTRY: &[ComponentUi] = &[
    ComponentUi {
        name: "Transform",
        type_name: "Transform",
        present: has::<Transform>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(Transform::from(MathTransform::IDENTITY));
            }
        },
        remove: strip::<Transform>,
        inspect: |world, entity, ui| {
            let Some(mut value) = transform_of(world, entity) else {
                return false;
            };
            if value.inspect(ui) {
                set_transform(world, entity, value);
                true
            } else {
                false
            }
        },
        copy_to: copy_component::<Transform>,
    },
    ComponentUi {
        name: "Camera",
        type_name: "Camera",
        present: has::<Camera>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(Camera::from(engine_renderer::Camera::new(
                    Vec3::new(0.0, 2.0, 6.0),
                    Vec3::ZERO,
                    16.0 / 9.0,
                )));
            }
        },
        remove: strip::<Camera>,
        inspect: |world, entity, ui| {
            let Some(mut value) = world.get::<Camera>(entity).map(|camera| camera.0) else {
                return false;
            };
            if value.inspect(ui) {
                if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                    entity_mut.insert(Camera::from(value));
                }
                true
            } else {
                false
            }
        },
        copy_to: copy_component::<Camera>,
    },
    ComponentUi {
        name: "Sprite",
        type_name: "Sprite",
        present: has::<Sprite>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(Sprite::new(
                    Vec2::splat(1.0),
                    engine_renderer::UvRect {
                        min: [0.0, 0.0],
                        max: [1.0, 1.0],
                    },
                ));
            }
        },
        remove: strip::<Sprite>,
        inspect: |world, entity, ui| {
            let Some(mut value) = world.get::<Sprite>(entity).copied() else {
                return false;
            };
            if value.inspect(ui) {
                if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                    entity_mut.insert(value);
                }
                true
            } else {
                false
            }
        },
        copy_to: copy_component::<Sprite>,
    },
    ComponentUi {
        name: "Asset Source",
        type_name: "AssetSource",
        present: has::<AssetSource>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(AssetSource::new(String::new()));
            }
        },
        remove: strip::<AssetSource>,
        inspect: |world, entity, ui| {
            let Some((mut path, had_id)) = world
                .get::<AssetSource>(entity)
                .map(|source| (source.path.clone(), source.id.clone()))
            else {
                return false;
            };

            let changed = ui
                .horizontal(|ui| {
                    ui.label("Path");
                    path.inspect_field(ui, 0.0)
                })
                .inner;

            // The stable id is read-only here — it comes from the asset's
            // `.meta` sidecar and the editor re-resolves it on load.
            match &had_id {
                Some(id) => {
                    ui.horizontal(|ui| {
                        ui.label("Id");
                        ui.add(
                            egui::Label::new(egui::RichText::new(id).monospace().weak()).truncate(),
                        );
                    });
                }
                None => {
                    ui.weak("No stable id — reference is path-only.");
                }
            }

            // A manual path edit re-points the reference: drop the id so
            // the next load doesn't heal the path back to the old file
            // (a backfill will re-adopt an id for the new path if known).
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(AssetSource { path, id: None });
            }
            changed
        },
        copy_to: copy_component::<AssetSource>,
    },
];

/// Draws the inspector for `selected`, or a placeholder message if
/// nothing (or a since-despawned entity) is selected.
///
/// Clicking a section's "Open Script" button sets `*open_script` to that
/// component's Rust type name; the host resolves it to a source location
/// (via rust-analyzer) and opens it in the code editor.
///
/// `others` are the selected entities besides the primary `selected`.
/// When non-empty a "N entities selected" line and an "Apply edits to
/// all" checkbox (bound to `batch_edit`) appear; with that checked, a
/// component field edited on the primary is copied to every `other`
/// entity that already has the same component.
pub fn show(
    ui: &mut egui::Ui,
    world: &mut World,
    selected: Option<Entity>,
    others: &[Entity],
    batch_edit: &mut bool,
    open_script: &mut Option<String>,
) {
    let Some(entity) = selected else {
        ui.label("No entity selected.");
        return;
    };
    if world.get_entity(entity).is_err() {
        ui.label("Selected entity no longer exists.");
        return;
    }

    let multi = !others.is_empty();
    if multi {
        ui.horizontal(|ui| {
            ui.weak(format!("{} entities selected", others.len() + 1));
            ui.checkbox(batch_edit, "Apply edits to all");
        });
        ui.separator();
    }
    let apply_to_all = multi && *batch_edit;

    identity_header(ui, world, entity);
    ui.separator();

    for component in REGISTRY {
        if !(component.present)(world, entity) {
            continue;
        }
        let mut remove = false;
        let mut changed = false;
        egui::CollapsingHeader::new(component.name)
            .id_salt(component.name)
            .default_open(true)
            .show(ui, |ui| {
                changed = (component.inspect)(world, entity, ui);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = true;
                    }
                    if ui
                        .small_button("Open Script")
                        .on_hover_text(format!(
                            "Jump to `{}`'s declaration (needs rust-analyzer)",
                            component.type_name
                        ))
                        .clicked()
                    {
                        *open_script = Some(component.type_name.to_string());
                    }
                });
            });
        if remove {
            (component.remove)(world, entity);
        } else if changed && apply_to_all {
            for &other in others {
                (component.copy_to)(world, entity, other);
            }
        }
    }

    ui.separator();
    add_component_menu(ui, world, entity);
}

/// The entity-identity block: id, editable name, and the Static /
/// Disabled markers.
fn identity_header(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    ui.horizontal(|ui| {
        ui.label("Name");
        // `Name` is a `String` newtype — reuse `String`'s `InspectField`.
        let mut name = name_of(world, entity).unwrap_or_default();
        if name.inspect_field(ui, 0.0) {
            set_name(world, entity, name);
        }
    });
    ui.horizontal(|ui| {
        ui.label("ID");
        ui.weak(format!("{entity:?}"));
    });
    ui.horizontal(|ui| {
        let mut is_static = has::<Static>(world, entity);
        if ui.checkbox(&mut is_static, "Static").changed() {
            toggle_marker::<Static>(world, entity, is_static);
        }
        let mut disabled = has::<Disabled>(world, entity);
        if ui.checkbox(&mut disabled, "Disabled").changed() {
            toggle_marker::<Disabled>(world, entity, disabled);
        }
        let mut locked = has::<Lock>(world, entity);
        if ui.checkbox(&mut locked, "Locked").changed() {
            toggle_marker::<Lock>(world, entity, locked);
        }
    });
}

/// Adds `T` when `present`, removes it otherwise. Despawn-safe.
fn toggle_marker<T: Component + Default>(world: &mut World, entity: Entity, present: bool) {
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    if present {
        entity_mut.insert(T::default());
    } else {
        entity_mut.remove::<T>();
    }
}

/// The Add-Component menu: every [`REGISTRY`] entry not already on the
/// entity.
fn add_component_menu(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    ui.menu_button("Add Component", |ui| {
        let mut any = false;
        for component in REGISTRY {
            if (component.present)(world, entity) {
                continue;
            }
            any = true;
            if ui.button(component.name).clicked() {
                (component.insert_default)(world, entity);
                ui.close();
            }
        }
        if !any {
            ui.label("Every known component is already on this entity.");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

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

    #[test]
    fn has_and_strip_track_a_component() {
        let mut world = World::new();
        let entity = world.spawn(Transform::from(MathTransform::IDENTITY)).id();
        assert!(has::<Transform>(&world, entity));
        strip::<Transform>(&mut world, entity);
        assert!(!has::<Transform>(&world, entity));
    }

    #[test]
    fn every_registry_entry_adds_and_removes_cleanly() {
        for component in REGISTRY {
            let mut world = World::new();
            let entity = world.spawn_empty().id();
            assert!(
                !(component.present)(&world, entity),
                "{} should start absent",
                component.name
            );
            (component.insert_default)(&mut world, entity);
            assert!(
                (component.present)(&world, entity),
                "{} should be present after insert_default",
                component.name
            );
            (component.remove)(&mut world, entity);
            assert!(
                !(component.present)(&world, entity),
                "{} should be gone after remove",
                component.name
            );
        }
    }

    #[test]
    fn copy_to_transfers_a_value_only_to_entities_that_have_the_component() {
        let mut world = World::new();
        let from = world
            .spawn(Transform::from(MathTransform::from_translation(Vec3::new(
                5.0, 0.0, 0.0,
            ))))
            .id();
        let has = world.spawn(Transform::from(MathTransform::IDENTITY)).id();
        let hasnt = world.spawn_empty().id();

        let transform_ui = REGISTRY
            .iter()
            .find(|c| c.name == "Transform")
            .expect("Transform entry");
        (transform_ui.copy_to)(&mut world, from, has);
        (transform_ui.copy_to)(&mut world, from, hasnt);

        assert_eq!(
            transform_of(&world, has).unwrap().translation,
            Vec3::new(5.0, 0.0, 0.0),
            "copied to the entity that already had a Transform"
        );
        assert!(
            transform_of(&world, hasnt).is_none(),
            "not added to an entity that lacked the component"
        );
    }

    #[test]
    fn registry_entries_have_unique_names() {
        for (i, a) in REGISTRY.iter().enumerate() {
            for b in &REGISTRY[i + 1..] {
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn toggle_marker_adds_and_removes() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        assert!(!has::<Static>(&world, entity));

        toggle_marker::<Static>(&mut world, entity, true);
        assert!(has::<Static>(&world, entity));

        toggle_marker::<Static>(&mut world, entity, false);
        assert!(!has::<Static>(&world, entity));
    }

    #[test]
    fn toggle_marker_on_a_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        world.despawn(entity);
        toggle_marker::<Static>(&mut world, entity, true); // must not panic
    }

    #[test]
    fn registry_type_names_are_valid_rust_identifiers() {
        for component in REGISTRY {
            assert!(
                !component.type_name.is_empty()
                    && component
                        .type_name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_'),
                "{} has a non-identifier type_name {:?}",
                component.name,
                component.type_name
            );
        }
    }

    #[test]
    fn sprite_default_is_a_unit_quad_full_atlas() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let sprite_ui = REGISTRY
            .iter()
            .find(|component| component.name == "Sprite")
            .expect("a Sprite registry entry");

        (sprite_ui.insert_default)(&mut world, entity);
        let sprite = world.get::<Sprite>(entity).copied().unwrap();
        assert_eq!(sprite.size, Vec2::splat(1.0));
        assert_eq!(sprite.uv.min, [0.0, 0.0]);
        assert_eq!(sprite.uv.max, [1.0, 1.0]);
        assert_eq!(sprite.color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn show_runs_with_the_open_script_out_param() {
        let mut world = World::new();
        let entity = world
            .spawn((Name::new("Cam"), Transform::from(MathTransform::IDENTITY)))
            .id();
        let mut open_script = None;
        egui::__run_test_ui(|ui| {
            show(
                ui,
                &mut world,
                Some(entity),
                &[],
                &mut false,
                &mut open_script,
            );
        });
        // No interaction in a headless pass, so nothing is requested.
        assert_eq!(open_script, None);
    }
}
