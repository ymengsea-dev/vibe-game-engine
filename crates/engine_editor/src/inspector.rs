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
    AssetSource, Camera, Disabled, Lock, MeshSource, Name, Script, Sprite, SpriteSource, Static,
    Transform, WaterVolume,
};
use engine_ecs::prelude::{Component, Entity, World};
use engine_utils::Transform as MathTransform;
use glam::{Vec2, Vec3};

use engine_scene::{
    AssetRef, AudioEmitterData, BodyKind, ColliderShape, GridRegion, MaterialOverrideData,
    PointLightData, RuleAction, RuleCompare, RuleCondition, RuleData, RuleTrigger,
    SceneAudioEmitter, SceneCollider, SceneMaterialOverride, SceneObjectId, ScenePointLight,
    SceneRules, SceneSpriteAnimation, SceneTilemap, SceneUi, SpriteAnimationData, UiAnchor,
    UiElementData, UiKind, WaterData,
};

use crate::assets::{AssetIndex, AssetKind};
use crate::inspect::{InspectField, Inspectable, number, property_row};

mod graph;

#[cfg(test)]
mod prototype_style_tests {
    #[test]
    fn component_sections_are_soft_filled_cards() {
        let frame = super::component_frame();
        assert_eq!(frame.fill, crate::theme::NIGHT.raised);
        assert_eq!(frame.stroke, egui::Stroke::NONE, "filled, not outlined");
        assert_eq!(frame.corner_radius, egui::CornerRadius::same(9));
    }
}

/// Validates a project-relative script reference before it enters the ECS.
pub fn valid_script_path(path: &std::path::Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && ((path.starts_with("src") && path.extension().and_then(|e| e.to_str()) == Some("rs"))
            || (path.starts_with("assets/scripts")
                && path.extension().and_then(|e| e.to_str()) == Some("lua")))
}

/// Whether an asset-browser payload is suitable for a typed inspector slot.
pub fn accepts_asset_kind(kind: AssetKind, expected: AssetKind) -> bool {
    kind == expected && kind.is_referenceable()
}

/// Produces a compact, allocation-only summary suitable for an inspector row.
pub fn collider_shape_summary(shape: &ColliderShape) -> String {
    match shape {
        ColliderShape::Ball { radius } => format!("Ball (radius {radius:.3})"),
        ColliderShape::Cuboid { half_extents } => format!(
            "Cuboid (half-extents [{:.3}, {:.3}, {:.3}])",
            half_extents[0], half_extents[1], half_extents[2]
        ),
        ColliderShape::Cylinder {
            half_height,
            radius,
        } => format!("Cylinder (half-height {half_height:.3}, radius {radius:.3})"),
        ColliderShape::Capsule {
            half_height,
            radius,
        } => format!("Capsule (half-height {half_height:.3}, radius {radius:.3})"),
        ColliderShape::TriMesh { .. } => "Triangle mesh".to_string(),
    }
}

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
    inspect: fn(&mut World, Entity, &mut egui::Ui, &AssetIndex) -> bool,
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

fn set_material_override(
    world: &mut World,
    entity: Entity,
    value: Option<MaterialOverrideData>,
) -> bool {
    if world.get::<MeshSource>(entity).is_none()
        || world
            .get::<SceneMaterialOverride>(entity)
            .map(|current| &current.0)
            == value.as_ref()
    {
        return false;
    }
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return false;
    };
    if let Some(value) = value {
        entity_mut.insert(SceneMaterialOverride(value));
    } else {
        entity_mut.remove::<SceneMaterialOverride>();
    }
    true
}

fn copy_material_override(world: &mut World, from: Entity, to: Entity) {
    let value = world
        .get::<SceneMaterialOverride>(from)
        .map(|value| value.0.clone());
    set_material_override(world, to, value);
}

fn set_sprite_image(
    world: &mut World,
    entity: Entity,
    assets: &AssetIndex,
    path: &std::path::Path,
) -> bool {
    if AssetKind::from_path(path) != AssetKind::Texture {
        return false;
    }
    let Some(id) = assets.id_for(path) else {
        return false;
    };
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return false;
    };
    let previous = entity_mut.get::<Sprite>().copied();
    let stored = entity_mut.get::<SpriteSource>().cloned();
    let replace_asset_source = stored.as_ref().is_some_and(|source| {
        entity_mut
            .get::<AssetSource>()
            .is_some_and(|asset| asset.id.as_deref() == Some(source.atlas.as_str()))
    });
    if stored
        .as_ref()
        .is_some_and(|source| source.atlas == id.to_string())
    {
        return false;
    }
    let size = previous.map_or_else(
        || {
            stored
                .as_ref()
                .map_or(Vec2::ONE, |source| Vec2::from(source.size))
        },
        |sprite| sprite.size,
    );
    let color = previous.map_or_else(
        || stored.as_ref().map_or([1.0; 4], |source| source.color),
        |sprite| sprite.color,
    );
    let z_order = previous.map_or_else(
        || stored.as_ref().map_or(0.0, |source| source.z_order),
        |sprite| sprite.z_order,
    );
    let region = stored
        .as_ref()
        .map_or_else(|| "full".to_owned(), |source| source.region.clone());
    let mut sprite = Sprite::new(
        size,
        engine_renderer::UvRect {
            min: [0.0; 2],
            max: [1.0; 2],
        },
    );
    sprite.color = color;
    sprite.z_order = z_order;
    entity_mut.insert((
        SpriteSource {
            atlas: id.to_string(),
            region,
            size: size.to_array(),
            color,
            z_order,
        },
        sprite,
    ));
    if replace_asset_source {
        entity_mut.insert(AssetSource::with_id(
            path.to_string_lossy().into_owned(),
            id.to_string(),
        ));
    }
    true
}

fn remove_sprite(world: &mut World, entity: Entity) {
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        let linked_source = entity_mut.get::<SpriteSource>().is_some_and(|source| {
            entity_mut
                .get::<AssetSource>()
                .is_some_and(|asset| asset.id.as_deref() == Some(source.atlas.as_str()))
        });
        entity_mut.remove::<(Sprite, SpriteSource, SceneSpriteAnimation)>();
        if linked_source {
            entity_mut.remove::<AssetSource>();
        }
    }
}

fn copy_sprite(world: &mut World, from: Entity, to: Entity) {
    let Some(source) = world.get::<SpriteSource>(from).cloned() else {
        return;
    };
    let Some(target_source) = world.get::<SpriteSource>(to) else {
        return;
    };
    let target_linked = world
        .get::<AssetSource>(to)
        .is_some_and(|asset| asset.id.as_deref() == Some(target_source.atlas.as_str()));
    let origin = world
        .get::<AssetSource>(from)
        .filter(|asset| asset.id.as_deref() == Some(source.atlas.as_str()))
        .cloned();
    let sprite = world.get::<Sprite>(from).copied();
    let animation = world.get::<SceneSpriteAnimation>(from).copied();
    if let Ok(mut entity_mut) = world.get_entity_mut(to) {
        entity_mut.insert(source);
        if let Some(sprite) = sprite {
            entity_mut.insert(sprite);
        } else {
            entity_mut.remove::<Sprite>();
        }
        if let Some(animation) = animation {
            entity_mut.insert(animation);
        } else {
            entity_mut.remove::<SceneSpriteAnimation>();
        }
        if target_linked {
            if let Some(origin) = origin {
                entity_mut.insert(origin);
            } else {
                entity_mut.remove::<AssetSource>();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RuleActionKind {
    Text,
    Audio,
    Event,
    Variable,
}

fn rule_target_matches(world: &World, entity: Entity, kind: RuleActionKind) -> bool {
    world.get::<Name>(entity).is_some()
        && world.get::<Disabled>(entity).is_none()
        && match kind {
            RuleActionKind::Text => world.get::<SceneUi>(entity).is_some(),
            RuleActionKind::Audio => world.get::<SceneAudioEmitter>(entity).is_some(),
            RuleActionKind::Event | RuleActionKind::Variable => false,
        }
}

fn ensure_rule_object_ids(world: &mut World, owner: Entity) {
    let missing: Vec<_> = world
        .iter_entities()
        .filter(|entity| {
            entity.get::<SceneObjectId>().is_none()
                && (entity.id() == owner
                    || (entity.get::<Name>().is_some()
                        && (entity.get::<SceneUi>().is_some()
                            || entity.get::<SceneAudioEmitter>().is_some())))
        })
        .map(|entity| entity.id())
        .collect();
    for entity in missing {
        world
            .entity_mut(entity)
            .insert(SceneObjectId(uuid::Uuid::new_v4().to_string()));
    }
}

fn add_visual_rule(
    world: &mut World,
    owner: Entity,
    kind: RuleActionKind,
    preferred_trigger: Option<RuleTrigger>,
) -> bool {
    let mut rules = world
        .get::<SceneRules>(owner)
        .map_or_else(Vec::new, |value| value.0.clone());
    if rules.len() >= 16 || visual_rule_count(world) >= 1024 {
        return false;
    }
    let owner_is_button = world
        .get::<SceneUi>(owner)
        .is_some_and(|ui| ui.0.kind == UiKind::Button);
    let trigger = preferred_trigger.unwrap_or_else(|| {
        if owner_is_button {
            RuleTrigger::UiClicked
        } else {
            RuleTrigger::Named("game_start".into())
        }
    });
    if matches!(trigger, RuleTrigger::UiClicked) && !owner_is_button {
        return false;
    }
    ensure_rule_object_ids(world, owner);
    let target = world
        .iter_entities()
        .find(|entity| rule_target_matches(world, entity.id(), kind))
        .and_then(|entity| entity.get::<SceneObjectId>())
        .map(|id| id.0.clone());
    let action = match kind {
        RuleActionKind::Text => {
            let Some(target) = target else { return false };
            RuleAction::SetUiText {
                target,
                text: "New text".into(),
            }
        }
        RuleActionKind::Audio => {
            let Some(target) = target else { return false };
            RuleAction::PlayAudio { target }
        }
        RuleActionKind::Event => RuleAction::EmitEvent {
            name: "next_event".into(),
        },
        RuleActionKind::Variable => RuleAction::AddToVariable {
            name: "score".into(),
            amount: 1.0,
        },
    };
    rules.push(RuleData {
        trigger,
        action,
        condition: None,
        graph_position: None,
    });
    world.entity_mut(owner).insert(SceneRules(rules));
    true
}

fn visual_rule_count(world: &World) -> usize {
    world
        .iter_entities()
        .filter_map(|entity| entity.get::<SceneRules>())
        .map(|rules| rules.0.len())
        .sum()
}

fn rule_targets(world: &World, kind: RuleActionKind) -> Vec<(String, String)> {
    world
        .iter_entities()
        .filter(|entity| rule_target_matches(world, entity.id(), kind))
        .filter_map(|entity| {
            let id = entity.get::<SceneObjectId>()?.0.clone();
            let name = entity.get::<Name>()?.0.clone();
            Some((id, name))
        })
        .collect()
}

fn rule_target_picker(
    ui: &mut egui::Ui,
    salt: (Entity, usize),
    target: &mut String,
    options: &[(String, String)],
) {
    let selected = options
        .iter()
        .find(|(id, _)| id == target)
        .map_or("Missing target", |(_, name)| name.as_str());
    choice(("visual_rule_target", salt), ui)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (id, name) in options {
                ui.selectable_value(target, id.clone(), name);
            }
        });
}

fn rule_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    index: usize,
    rule: &mut RuleData,
    owner_is_button: bool,
    text_targets: &[(String, String)],
    audio_targets: &[(String, String)],
) {
    property_row(ui, "When", |ui| {
        choice(("visual_rule_trigger", entity, index), ui)
            .selected_text(match &rule.trigger {
                RuleTrigger::UiClicked => "This button is clicked",
                RuleTrigger::Named(_) => "Named event",
            })
            .show_ui(ui, |ui| {
                if owner_is_button
                    && ui
                        .selectable_label(
                            matches!(rule.trigger, RuleTrigger::UiClicked),
                            "This button is clicked",
                        )
                        .clicked()
                {
                    rule.trigger = RuleTrigger::UiClicked;
                }
                if ui
                    .selectable_label(matches!(rule.trigger, RuleTrigger::Named(_)), "Named event")
                    .clicked()
                    && !matches!(rule.trigger, RuleTrigger::Named(_))
                {
                    rule.trigger = RuleTrigger::Named("game_start".into());
                }
            });
    });
    if let RuleTrigger::Named(name) = &mut rule.trigger {
        property_row(ui, "Event", |ui| {
            crate::widgets::text_field(ui, name, "Event name");
        });
    }
    match &mut rule.action {
        RuleAction::SetUiText { target, text } => {
            property_row(ui, "Set text on", |ui| {
                rule_target_picker(ui, (entity, index), target, text_targets);
            });
            property_row(ui, "Text", |ui| {
                crate::widgets::text_field(ui, text, "");
            });
            note(ui, "Write {score} to show a variable's value.");
        }
        RuleAction::PlayAudio { target } => {
            property_row(ui, "Play sound on", |ui| {
                rule_target_picker(ui, (entity, index), target, audio_targets);
            });
        }
        RuleAction::EmitEvent { name } => {
            property_row(ui, "Emit event", |ui| {
                crate::widgets::text_field(ui, name, "Event name");
            });
        }
        RuleAction::SetVariable { .. } | RuleAction::AddToVariable { .. } => {
            property_row(ui, "Do", |ui| {
                variable_action_fields(ui, (entity, index), &mut rule.action);
            });
        }
    }
    rule_condition_fields(ui, (entity, index), &mut rule.condition);
}

fn variable_action_fields(ui: &mut egui::Ui, salt: (Entity, usize), action: &mut RuleAction) {
    let adding = matches!(action, RuleAction::AddToVariable { .. });
    egui::ComboBox::from_id_salt(("visual_rule_variable_op", salt))
        .width(72.0)
        .icon(crate::widgets::combo_chevron)
        .selected_text(if adding { "Add to" } else { "Set" })
        .show_ui(ui, |ui| {
            let (name, number) = match action {
                RuleAction::SetVariable { name, value } => (name.clone(), *value),
                RuleAction::AddToVariable { name, amount } => (name.clone(), *amount),
                _ => return,
            };
            if ui.selectable_label(!adding, "Set").clicked() && adding {
                *action = RuleAction::SetVariable {
                    name,
                    value: number,
                };
            } else if ui.selectable_label(adding, "Add to").clicked() && !adding {
                *action = RuleAction::AddToVariable {
                    name,
                    amount: number,
                };
            }
        });
    let (name, value, preposition) = match action {
        RuleAction::SetVariable { name, value } => (name, value, "to"),
        RuleAction::AddToVariable { name, amount } => (name, amount, "by"),
        _ => return,
    };
    let height = ui.spacing().interact_size.y;
    let field = crate::widgets::field(ui, name, 12.0, height)
        .hint_text("Variable")
        .desired_width(90.0);
    ui.add(field);
    ui.label(preposition);
    number(ui, egui::DragValue::new(value).speed(0.1));
}

fn rule_condition_fields(
    ui: &mut egui::Ui,
    salt: (Entity, usize),
    condition: &mut Option<RuleCondition>,
) {
    property_row(ui, "Only if", |ui| {
        let mut enabled = condition.is_some();
        if crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut enabled, "")
            .on_hover_text("Run this rule only when a variable matches")
            .changed()
        {
            *condition = enabled.then(|| RuleCondition {
                variable: "score".into(),
                compare: RuleCompare::GreaterOrEqual,
                value: 1.0,
            });
        }
        let Some(condition) = condition else {
            return;
        };
        let height = ui.spacing().interact_size.y;
        let field = crate::widgets::field(ui, &mut condition.variable, 12.0, height)
            .hint_text("Variable")
            .desired_width(90.0);
        ui.add(field);
        egui::ComboBox::from_id_salt(("visual_rule_compare", salt))
            .width(52.0)
            .icon(crate::widgets::combo_chevron)
            .selected_text(condition.compare.symbol())
            .show_ui(ui, |ui| {
                for compare in RuleCompare::ALL {
                    ui.selectable_value(&mut condition.compare, compare, compare.symbol());
                }
            });
        number(ui, egui::DragValue::new(&mut condition.value).speed(0.1));
    });
}

fn fit_sprite_collider(world: &mut World, entity: Entity) -> bool {
    let size = world
        .get::<Sprite>(entity)
        .map(|sprite| sprite.size.to_array())
        .or_else(|| world.get::<SpriteSource>(entity).map(|source| source.size));
    let Some([width, height]) = size else {
        return false;
    };
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return false;
    }
    let body = world
        .get::<SceneCollider>(entity)
        .map_or(BodyKind::Fixed, |collider| collider.0.body);
    let data = engine_scene::ColliderData {
        shape: ColliderShape::Cuboid {
            half_extents: [width * 0.5, height * 0.5, 0.05],
        },
        body,
    };
    if world
        .get::<SceneCollider>(entity)
        .is_some_and(|collider| collider.0 == data)
    {
        return false;
    }
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return false;
    };
    entity_mut.insert(SceneCollider(data));
    true
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
        inspect: |world, entity, ui, _assets| {
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
        inspect: |world, entity, ui, _assets| {
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
        name: "Point Light",
        type_name: "ScenePointLight",
        present: has::<ScenePointLight>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(ScenePointLight(PointLightData::default()));
            }
        },
        remove: strip::<ScenePointLight>,
        inspect: |world, entity, ui, _assets| {
            let Some(mut data) = world.get::<ScenePointLight>(entity).map(|light| light.0) else {
                return false;
            };
            let mut changed = property_row(ui, "Color", |ui| {
                ui.color_edit_button_rgb(&mut data.color).changed()
            });
            for (label, value, range) in [
                ("Intensity", &mut data.intensity, 0.0..=1000.0),
                ("Range", &mut data.range, 0.01..=10_000.0),
            ] {
                changed |= property_row(ui, label, |ui| slider(ui, value, range));
            }
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(ScenePointLight(data));
            }
            changed
        },
        copy_to: copy_component::<ScenePointLight>,
    },
    ComponentUi {
        name: "Water",
        type_name: "WaterVolume",
        present: has::<WaterVolume>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(WaterVolume {
                    surface: engine_physics::WaterSurface::default(),
                });
            }
        },
        remove: strip::<WaterVolume>,
        inspect: |world, entity, ui, _assets| {
            let Some(mut data) = world
                .get::<WaterVolume>(entity)
                .map(|volume| WaterData::from(volume.surface))
            else {
                return false;
            };
            let mut changed = false;
            property_row(ui, "Center", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(&mut data.center[0])
                        .prefix("X ")
                        .speed(0.1),
                )
                .changed();
                changed |= number(
                    ui,
                    egui::DragValue::new(&mut data.center[2])
                        .prefix("Z ")
                        .speed(0.1),
                )
                .changed();
            });
            property_row(ui, "Level", |ui| {
                changed |=
                    number(ui, egui::DragValue::new(&mut data.base_height).speed(0.1)).changed();
            });
            property_row(ui, "Half size", |ui| {
                for (axis, value) in ["W ", "D "].into_iter().zip(&mut data.half_extent) {
                    changed |= number(
                        ui,
                        egui::DragValue::new(value)
                            .prefix(axis)
                            .speed(0.1)
                            .range(0.01..=10000.0),
                    )
                    .changed();
                }
            });
            subsection(ui, "Waves and physics", |ui| {
                property_row(ui, "Direction", |ui| {
                    for (axis, value) in ["X ", "Z "].into_iter().zip(&mut data.wave_direction) {
                        changed |= number(
                            ui,
                            egui::DragValue::new(value)
                                .prefix(axis)
                                .speed(0.05)
                                .range(-1.0..=1.0),
                        )
                        .changed();
                    }
                });
                for (label, value, speed, range) in [
                    ("Amplitude", &mut data.wave_amplitude, 0.01, 0.0..=100.0),
                    ("Length", &mut data.wave_length, 0.1, 0.01..=10000.0),
                    (
                        "Speed",
                        &mut data.wave_speed,
                        0.05,
                        f32::NEG_INFINITY..=f32::INFINITY,
                    ),
                ] {
                    property_row(ui, label, |ui| {
                        changed |=
                            number(ui, egui::DragValue::new(value).speed(speed).range(range))
                                .changed();
                    });
                }
                property_row(ui, "Current", |ui| {
                    for (axis, value) in ["X ", "Y ", "Z "].into_iter().zip(&mut data.current) {
                        changed |= number(ui, egui::DragValue::new(value).prefix(axis).speed(0.05))
                            .changed();
                    }
                });
                for (label, value, speed, range) in [
                    ("Density", &mut data.density, 10.0, 0.01..=100000.0),
                    ("Drag", &mut data.linear_drag, 0.05, 0.0..=1000.0),
                ] {
                    property_row(ui, label, |ui| {
                        changed |=
                            number(ui, egui::DragValue::new(value).speed(speed).range(range))
                                .changed();
                    });
                }
            });
            if !changed {
                return false;
            }
            match engine_physics::WaterSurface::try_from(data) {
                Ok(surface) => {
                    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                        entity_mut.insert(WaterVolume { surface });
                    }
                    true
                }
                Err(error) => {
                    ui.colored_label(crate::theme::NIGHT.danger, error.to_string());
                    false
                }
            }
        },
        copy_to: copy_component::<WaterVolume>,
    },
    ComponentUi {
        name: "Audio Emitter",
        type_name: "SceneAudioEmitter",
        present: has::<SceneAudioEmitter>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                // An empty asset id: the emitter is placed first and
                // pointed at a sound after, which is the order a person
                // works in. It resolves to silence until then.
                entity_mut.insert(SceneAudioEmitter(AudioEmitterData::new(AssetRef {
                    id: String::new(),
                })));
            }
        },
        remove: strip::<SceneAudioEmitter>,
        inspect: |world, entity, ui, assets| {
            let Some(mut value) = world
                .get::<SceneAudioEmitter>(entity)
                .map(|carrier| carrier.0.clone())
            else {
                return false;
            };
            let mut changed = false;
            property_row(ui, "Sound", |ui| {
                changed |= value.sound.id.inspect_field(ui, 0.0);
            });
            if let Some(dropped) = released_asset(ui, AssetKind::Audio) {
                if set_emitter_sound_from_drop(&mut value, assets, &dropped) {
                    changed = true;
                } else {
                    note(ui, "Import this audio file before assigning it.");
                }
            }
            property_row(ui, "Playback", |ui| {
                let palette = crate::theme::NIGHT;
                ui.spacing_mut().item_spacing.x = 14.0;
                changed |= crate::widgets::checkbox(ui, &palette, &mut value.autoplay, "Autoplay")
                    .changed();
                changed |=
                    crate::widgets::checkbox(ui, &palette, &mut value.looping, "Loop").changed();
            });
            property_row(ui, "Gain", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(&mut value.gain)
                        .speed(0.05)
                        .range(0.0..=4.0),
                )
                .changed();
            });
            property_row(ui, "Radius", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(&mut value.radius)
                        .speed(0.25)
                        .range(0.0..=1000.0),
                )
                .changed();
            });
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(SceneAudioEmitter(value));
            }
            changed
        },
        copy_to: copy_component::<SceneAudioEmitter>,
    },
    ComponentUi {
        name: "Collider",
        type_name: "SceneCollider",
        present: has::<SceneCollider>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(SceneCollider(engine_scene::ColliderData {
                    shape: ColliderShape::Cuboid {
                        half_extents: [0.5, 0.5, 0.5],
                    },
                    body: BodyKind::Fixed,
                }));
            }
        },
        remove: strip::<SceneCollider>,
        inspect: |world, entity, ui, _assets| {
            let Some(mut value) = world.get::<SceneCollider>(entity).cloned() else {
                return false;
            };
            let mut changed = false;
            let has_sprite = world.get::<SpriteSource>(entity).is_some()
                || world.get::<Sprite>(entity).is_some();
            property_row(ui, "Body", |ui| {
                choice(("collider_body", entity), ui)
                    .selected_text(format!("{:?}", value.0.body))
                    .show_ui(ui, |ui| {
                        for body in [BodyKind::Fixed, BodyKind::Dynamic, BodyKind::Kinematic] {
                            changed |= ui
                                .selectable_value(&mut value.0.body, body, format!("{body:?}"))
                                .changed();
                        }
                    });
            });
            property_row(ui, "Shape", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(shape_name(&value.0.shape))
                            .color(crate::theme::NIGHT.text),
                    )
                    .truncate(),
                )
                .on_hover_text(collider_shape_summary(&value.0.shape));
            });
            let dimension = |ui: &mut egui::Ui, label: &str, value: &mut f32| {
                property_row(ui, label, |ui| {
                    number(
                        ui,
                        egui::DragValue::new(value)
                            .speed(0.05)
                            .range(0.001..=10000.0),
                    )
                    .changed()
                })
            };
            match &mut value.0.shape {
                ColliderShape::Ball { radius } => changed |= dimension(ui, "Radius", radius),
                ColliderShape::Cuboid { half_extents } => {
                    property_row(ui, "Half size", |ui| {
                        let mut values = *half_extents;
                        for (axis, extent) in ["X ", "Y ", "Z "].into_iter().zip(&mut values) {
                            changed |= number(
                                ui,
                                egui::DragValue::new(extent)
                                    .prefix(axis)
                                    .speed(0.05)
                                    .range(0.001..=10000.0),
                            )
                            .changed();
                        }
                        *half_extents = values;
                    });
                }
                ColliderShape::Cylinder {
                    half_height,
                    radius,
                }
                | ColliderShape::Capsule {
                    half_height,
                    radius,
                } => {
                    changed |= dimension(ui, "Half height", half_height);
                    changed |= dimension(ui, "Radius", radius);
                }
                ColliderShape::TriMesh { mesh } => {
                    property_row(ui, "Mesh", |ui| {
                        ui.add(egui::Label::new(mesh.id.as_str()).truncate());
                    });
                    if released_asset(ui, AssetKind::Mesh).is_some() {
                        note(
                            ui,
                            "Mesh drop received; import resolution will assign its stable id.",
                        );
                    }
                }
            }
            let fit_sprite = has_sprite && ui.button("Fit box to sprite").clicked();
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(value);
            }
            if fit_sprite {
                changed |= fit_sprite_collider(world, entity);
            }
            changed
        },
        copy_to: copy_component::<SceneCollider>,
    },
    ComponentUi {
        name: "Sprite",
        type_name: "Sprite",
        present: |world, entity| has::<Sprite>(world, entity) || has::<SpriteSource>(world, entity),
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
        remove: remove_sprite,
        inspect: |world, entity, ui, assets| {
            let source = world.get::<SpriteSource>(entity).cloned();
            let live = world.get::<Sprite>(entity).copied();
            let image = source
                .as_ref()
                .and_then(|source| crate::assets::parse_asset_id(&source.atlas))
                .and_then(|id| assets.path_for(id))
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("No image — drop a PNG or JPEG");
            let dropped = sprite_image_slot(ui, image);
            let mut value = live.unwrap_or_else(|| {
                let mut sprite = Sprite::new(
                    source
                        .as_ref()
                        .map_or(Vec2::ONE, |source| Vec2::from(source.size)),
                    engine_renderer::UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                );
                if let Some(source) = &source {
                    sprite.color = source.color;
                    sprite.z_order = source.z_order;
                }
                sprite
            });
            let mut changed = false;
            if value.inspect(ui) {
                if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                    if live.is_some() {
                        entity_mut.insert(value);
                    }
                    if let Some(mut source) = source {
                        source.size = value.size.to_array();
                        source.color = value.color;
                        source.z_order = value.z_order;
                        entity_mut.insert(source);
                    }
                }
                changed = true;
            }
            if let Some(mut source) = world.get::<SpriteSource>(entity).cloned() {
                let parsed = GridRegion::parse(&source.region);
                let mut use_grid = parsed.is_some();
                let toggled = property_row(ui, "Sheet", |ui| {
                    crate::widgets::checkbox(
                        ui,
                        &crate::theme::NIGHT,
                        &mut use_grid,
                        "Use one cell",
                    )
                    .on_hover_text(
                        "Divide the image into equal columns and rows, then choose one cell",
                    )
                    .changed()
                });
                let region = if use_grid {
                    let mut grid = parsed.unwrap_or(GridRegion {
                        columns: 2,
                        rows: 2,
                        column: 0,
                        row: 0,
                    });
                    property_row(ui, "Columns", |ui| {
                        number(ui, egui::DragValue::new(&mut grid.columns).range(1..=4096));
                    });
                    property_row(ui, "Rows", |ui| {
                        number(ui, egui::DragValue::new(&mut grid.rows).range(1..=4096));
                    });
                    grid.column = grid.column.min(grid.columns.saturating_sub(1));
                    grid.row = grid.row.min(grid.rows.saturating_sub(1));
                    property_row(ui, "Cell", |ui| {
                        number(
                            ui,
                            egui::DragValue::new(&mut grid.column)
                                .range(0..=grid.columns - 1)
                                .prefix("X "),
                        );
                        number(
                            ui,
                            egui::DragValue::new(&mut grid.row)
                                .range(0..=grid.rows - 1)
                                .prefix("Y "),
                        );
                    });
                    Some(grid.name())
                } else if toggled {
                    Some("full".to_owned())
                } else {
                    None
                };
                if let Some(region) = region
                    && region != source.region
                {
                    source.region = region;
                    world.entity_mut(entity).insert(source.clone());
                    changed = true;
                }
                if let Some(grid) = GridRegion::parse(&source.region) {
                    let previous = world.get::<SceneSpriteAnimation>(entity).copied();
                    let mut enabled = previous.is_some();
                    property_row(ui, "Animate", |ui| {
                        crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut enabled, "")
                            .on_hover_text(
                                "Play consecutive cells from the selected cell in Play mode",
                            );
                    });
                    if enabled {
                        let mut data = previous.map_or(
                            SpriteAnimationData {
                                frames: 2,
                                fps: 8.0,
                                looping: true,
                            },
                            |animation| animation.data,
                        );
                        let first =
                            u64::from(grid.row) * u64::from(grid.columns) + u64::from(grid.column);
                        let available = (u64::from(grid.columns) * u64::from(grid.rows) - first)
                            .min(4096) as u32;
                        data.frames = data.frames.clamp(1, available);
                        property_row(ui, "Frames", |ui| {
                            number(
                                ui,
                                egui::DragValue::new(&mut data.frames).range(1..=available),
                            );
                        });
                        property_row(ui, "Speed", |ui| {
                            number(
                                ui,
                                egui::DragValue::new(&mut data.fps)
                                    .speed(0.1)
                                    .range(0.1..=120.0)
                                    .suffix(" fps"),
                            );
                        });
                        property_row(ui, "Loop", |ui| {
                            crate::widgets::checkbox(
                                ui,
                                &crate::theme::NIGHT,
                                &mut data.looping,
                                "",
                            );
                        });
                        if previous.is_none_or(|animation| animation.data != data) {
                            world
                                .entity_mut(entity)
                                .insert(SceneSpriteAnimation::new(data));
                            changed = true;
                        }
                    } else if previous.is_some() {
                        world.entity_mut(entity).remove::<SceneSpriteAnimation>();
                        changed = true;
                    }
                } else if world.get::<SceneSpriteAnimation>(entity).is_some() {
                    world.entity_mut(entity).remove::<SceneSpriteAnimation>();
                    changed = true;
                }
            }
            if world.get::<SceneCollider>(entity).is_none()
                && ui.button("Add box collision").clicked()
            {
                changed |= fit_sprite_collider(world, entity);
            }
            if let Some(path) = dropped {
                changed |= set_sprite_image(world, entity, assets, &path);
            }
            changed
        },
        copy_to: copy_sprite,
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
        inspect: |world, entity, ui, _assets| {
            let Some((mut path, had_id)) = world
                .get::<AssetSource>(entity)
                .map(|source| (source.path.clone(), source.id.clone()))
            else {
                return false;
            };

            let changed = property_row(ui, "Path", |ui| path.inspect_field(ui, 0.0));
            let mut changed = changed;
            if let Some(dropped) = released_asset(ui, AssetKind::Mesh) {
                path = dropped.to_string_lossy().into_owned();
                changed = true;
            }

            // The stable id is read-only here — it comes from the asset's
            // `.meta` sidecar and the editor re-resolves it on load.
            match &had_id {
                Some(id) => {
                    property_row(ui, "ID", |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(id).color(crate::theme::NIGHT.subtle),
                            )
                            .truncate(),
                        );
                    });
                }
                None => {
                    note(ui, "No stable ID yet, so the reference uses the path only.");
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
    ComponentUi {
        name: "Game UI",
        type_name: "SceneUi",
        present: has::<SceneUi>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(SceneUi(UiElementData::label()));
                if entity_mut.get::<engine_scene::SceneObjectId>().is_none() {
                    entity_mut.insert(engine_scene::SceneObjectId(
                        uuid::Uuid::new_v4().to_string(),
                    ));
                }
            }
        },
        remove: strip::<SceneUi>,
        inspect: |world, entity, ui, _assets| {
            let Some(mut data) = world.get::<SceneUi>(entity).map(|value| value.0.clone()) else {
                return false;
            };
            let mut changed = false;
            property_row(ui, "Widget", |ui| {
                choice(("game_ui_widget", entity), ui)
                    .selected_text(match data.kind {
                        UiKind::Label => "Label",
                        UiKind::Button => "Button",
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut data.kind, UiKind::Label, "Label")
                            .changed()
                        {
                            data.color = UiElementData::label().color;
                            changed = true;
                        }
                        if ui
                            .selectable_value(&mut data.kind, UiKind::Button, "Button")
                            .changed()
                        {
                            data.color = UiElementData::button().color;
                            changed = true;
                        }
                    });
            });
            property_row(ui, "Text", |ui| {
                changed |= crate::widgets::text_field(ui, &mut data.text, "").changed();
            });
            property_row(ui, "Anchor", |ui| {
                choice(("game_ui_anchor", entity), ui)
                    .selected_text(format!("{:?}", data.anchor))
                    .show_ui(ui, |ui| {
                        for anchor in [
                            UiAnchor::TopLeft,
                            UiAnchor::Top,
                            UiAnchor::TopRight,
                            UiAnchor::Left,
                            UiAnchor::Center,
                            UiAnchor::Right,
                            UiAnchor::BottomLeft,
                            UiAnchor::Bottom,
                            UiAnchor::BottomRight,
                        ] {
                            changed |= ui
                                .selectable_value(&mut data.anchor, anchor, format!("{anchor:?}"))
                                .changed();
                        }
                    });
            });
            for (label, values) in [("Offset", &mut data.offset), ("Size", &mut data.size)] {
                property_row(ui, label, |ui| {
                    for value in values {
                        let mut control = egui::DragValue::new(value).speed(1.0);
                        if label == "Size" {
                            control = control.range(1.0..=10000.0);
                        }
                        changed |= number(ui, control).changed();
                    }
                });
            }
            property_row(
                ui,
                if data.kind == UiKind::Label {
                    "Text color"
                } else {
                    "Background"
                },
                |ui| {
                    changed |= ui
                        .color_edit_button_rgba_unmultiplied(&mut data.color)
                        .changed();
                },
            );
            if data.kind == UiKind::Button
                && let Some(id) = world.get::<engine_scene::SceneObjectId>(entity)
            {
                property_row(ui, "Click ID", |ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(id.0.clone());
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(id.0.as_str()).color(crate::theme::NIGHT.subtle),
                        )
                        .truncate(),
                    );
                });
            }
            if data.text.len() > 1024 {
                ui.colored_label(
                    crate::theme::NIGHT.danger,
                    "Text is too long for a scene UI element",
                );
                return false;
            }
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(SceneUi(data));
            }
            changed
        },
        copy_to: copy_component::<SceneUi>,
    },
    ComponentUi {
        name: "Script",
        type_name: "Script",
        present: has::<Script>,
        insert_default: |world, entity| {
            if let Ok(mut e) = world.get_entity_mut(entity) {
                e.insert(Script::default());
            }
        },
        remove: strip::<Script>,
        inspect: |world, entity, ui, _assets| {
            let Some(mut value) = world.get::<Script>(entity).cloned() else {
                return false;
            };
            let mut path = value.path.clone();
            let changed = property_row(ui, "File", |ui| path.inspect_field(ui, 0.0));
            if !path.is_empty() && !valid_script_path(std::path::Path::new(&path)) {
                ui.colored_label(
                    crate::theme::NIGHT.danger,
                    "Expected src/*.rs or assets/scripts/*.lua",
                );
            }
            if changed {
                value.path = path;
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.insert(value);
                }
            }
            changed
        },
        copy_to: copy_component::<Script>,
    },
];

fn set_emitter_sound_from_drop(
    emitter: &mut AudioEmitterData,
    assets: &AssetIndex,
    path: &std::path::Path,
) -> bool {
    if AssetKind::from_path(path) != AssetKind::Audio {
        return false;
    }
    let Some(id) = assets.id_for(path) else {
        return false;
    };
    let id = id.to_string();
    if emitter.sound.id == id {
        return false;
    }
    emitter.sound.id = id;
    true
}

fn released_asset(ui: &mut egui::Ui, expected: AssetKind) -> Option<std::path::PathBuf> {
    let response =
        ui.allocate_response(egui::vec2(ui.available_width(), 20.0), egui::Sense::hover());
    if let Some(path) = response.dnd_release_payload::<std::path::PathBuf>()
        && accepts_asset_kind(AssetKind::from_path(&path), expected)
    {
        return Some((*path).clone());
    }
    if response.hovered() {
        ui.weak(format!("Drop {} asset here", expected.label()));
    }
    None
}

fn sprite_image_slot(ui: &mut egui::Ui, name: &str) -> Option<std::path::PathBuf> {
    property_row(ui, "Image", |ui| image_drop_zone(ui, name, None))
}

/// A soft drop target for an image from Project Assets: `text`, with an
/// optional `hint` under it, and it lights up while an asset is dragged
/// over it. Returns the dropped image.
fn image_drop_zone(
    ui: &mut egui::Ui,
    text: &str,
    hint: Option<&str>,
) -> Option<std::path::PathBuf> {
    let palette = crate::theme::NIGHT;
    let height = if hint.is_some() { 48.0 } else { 30.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let dragging = response.dnd_hover_payload::<std::path::PathBuf>().is_some();
    let (fill, ink) = if dragging {
        (palette.accent_fill, palette.text)
    } else {
        (palette.raised, palette.muted)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 9.0, fill);
    let room = rect.width() - 16.0;
    let title = crate::widgets::one_line(painter, text, 12.0, ink, room);
    let note = hint.map(|hint| crate::widgets::one_line(painter, hint, 11.5, palette.subtle, room));
    let gap = 3.0;
    let total = title.size().y + note.as_ref().map_or(0.0, |note| gap + note.size().y);
    let mut y = rect.center().y - total / 2.0;
    let title_height = title.size().y;
    painter.galley(
        egui::pos2(rect.center().x - title.size().x / 2.0, y),
        title,
        ink,
    );
    y += title_height + gap;
    if let Some(note) = note {
        painter.galley(
            egui::pos2(rect.center().x - note.size().x / 2.0, y),
            note,
            palette.subtle,
        );
    }
    response
        .dnd_release_payload::<std::path::PathBuf>()
        .filter(|path| AssetKind::from_path(path) == AssetKind::Texture)
        .map(|path| (*path).clone())
}

/// Draws the inspector for `selected`, or a placeholder message if
/// nothing (or a since-despawned entity) is selected.
///
/// Choosing "Open script" in a section's "⋯" menu sets `*open_script` to that
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
    assets: &AssetIndex,
    open_script: &mut Option<String>,
) {
    let palette = crate::theme::NIGHT;
    let Some(entity) = selected else {
        ui.label(egui::RichText::new("No entity selected.").color(palette.muted));
        return;
    };
    if world.get_entity(entity).is_err() {
        ui.label(egui::RichText::new("Selected entity no longer exists.").color(palette.muted));
        return;
    }
    inspector_style(ui.style_mut());

    identity_header(ui, world, entity);
    let multi = !others.is_empty();
    if multi {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("{} entities selected", others.len() + 1))
                    .color(palette.muted),
            );
            crate::widgets::checkbox(ui, &palette, batch_edit, "Apply edits to all");
        });
    }
    let apply_to_all = multi && *batch_edit;
    ui.add_space(6.0);

    for component in REGISTRY {
        if !(component.present)(world, entity) {
            continue;
        }
        let mut remove = false;
        let mut changed = false;
        section(
            ui,
            component.name,
            Some(|ui: &mut egui::Ui| {
                if ui
                    .button("Open script")
                    .on_hover_text(format!(
                        "Jump to `{}`'s declaration (needs rust-analyzer)",
                        component.type_name
                    ))
                    .clicked()
                {
                    *open_script = Some(component.type_name.to_string());
                    ui.close();
                }
                if ui
                    .button(egui::RichText::new("Remove component").color(palette.danger))
                    .clicked()
                {
                    remove = true;
                    ui.close();
                }
            }),
            |ui| changed = (component.inspect)(world, entity, ui, assets),
        );
        if remove {
            (component.remove)(world, entity);
        } else if changed && apply_to_all {
            for &other in others {
                (component.copy_to)(world, entity, other);
            }
        }
    }

    if let Some(mut data) = world
        .get::<SceneTilemap>(entity)
        .map(|source| source.0.clone())
    {
        section(ui, "Tilemap", None::<fn(&mut egui::Ui)>, |ui| {
            let image = crate::assets::parse_asset_id(&data.atlas.id)
                .and_then(|id| assets.path_for(id))
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("Missing image");
            property_row(ui, "Image", |ui| {
                ui.add(egui::Label::new(image).truncate());
            });
            note(ui, "Paint or erase from Viewport → Tilemap in 2D mode.");
            property_row(ui, "Columns", |ui| {
                number(ui, egui::DragValue::new(&mut data.columns).range(1..=4096));
            });
            property_row(ui, "Rows", |ui| {
                number(ui, egui::DragValue::new(&mut data.rows).range(1..=4096));
            });
            data.brush_column = data.brush_column.min(data.columns - 1);
            data.brush_row = data.brush_row.min(data.rows - 1);
            property_row(ui, "Tile size", |ui| {
                for value in &mut data.cell_size {
                    number(ui, egui::DragValue::new(value).range(0.01..=10_000.0));
                }
            });
            property_row(ui, "Brush cell", |ui| {
                number(
                    ui,
                    egui::DragValue::new(&mut data.brush_column)
                        .range(0..=data.columns - 1)
                        .prefix("X "),
                );
                number(
                    ui,
                    egui::DragValue::new(&mut data.brush_row)
                        .range(0..=data.rows - 1)
                        .prefix("Y "),
                );
            });
            property_row(ui, "Solid", |ui| {
                crate::widgets::checkbox(
                    ui,
                    &palette,
                    &mut data.brush_collision,
                    "Paint solid tiles",
                )
                .on_hover_text(
                    "Add fixed collision to painted cells; repaint with this off to clear it",
                );
            });
            if let Err(error) = crate::tilemap::update_settings(world, entity, data.clone()) {
                ui.colored_label(palette.danger, error);
            }
        });
    }

    let mut rules = world
        .get::<SceneRules>(entity)
        .map_or_else(Vec::new, |value| value.0.clone());
    if !rules.is_empty() {
        ensure_rule_object_ids(world, entity);
    }
    let before = rules.clone();
    let owner_is_button = world
        .get::<SceneUi>(entity)
        .is_some_and(|value| value.0.kind == UiKind::Button);
    let text_targets = rule_targets(world, RuleActionKind::Text);
    let audio_targets = rule_targets(world, RuleActionKind::Audio);
    let has_text_target = world
        .iter_entities()
        .any(|candidate| rule_target_matches(world, candidate.id(), RuleActionKind::Text));
    let has_audio_target = world
        .iter_entities()
        .any(|candidate| rule_target_matches(world, candidate.id(), RuleActionKind::Audio));
    let can_add_rule = rules.len() < 16 && visual_rule_count(world) < 1024;
    let mut add_rule = None;
    let mut remove_rule = None;
    section(ui, "Visual Rules", None::<fn(&mut egui::Ui)>, |ui| {
        note(ui, "React to this button or to a named gameplay event.");
        ui.horizontal_wrapped(|ui| {
            let add = egui::Button::new(egui::RichText::new("Add rule").color(if can_add_rule {
                palette.accent
            } else {
                palette.subtle
            }))
            .fill(palette.hover);
            ui.add_enabled_ui(can_add_rule, |ui| {
                egui::containers::menu::MenuButton::from_button(add).ui(ui, |ui| {
                    crate::theme::style_context_menu(ui);
                    for (label, kind, enabled) in [
                        ("Set UI text", RuleActionKind::Text, has_text_target),
                        ("Play a sound", RuleActionKind::Audio, has_audio_target),
                        ("Emit an event", RuleActionKind::Event, true),
                        ("Change a variable", RuleActionKind::Variable, true),
                    ] {
                        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                            add_rule = Some((kind, None));
                            ui.close();
                        }
                    }
                });
            });
            if ui.button("Open gameplay graph").clicked() {
                graph::open(ui.ctx(), entity);
            }
        });
        for (index, rule) in rules.iter_mut().enumerate() {
            egui::Frame::NONE
                .fill(palette.panel)
                .corner_radius(7)
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 4,
                    top: 2,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(crate::theme::semibold(format!("Rule {}", index + 1)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let more = crate::widgets::more_button(ui, &palette);
                            egui::Popup::menu(&more).show(|ui| {
                                crate::theme::style_context_menu(ui);
                                if ui
                                    .button(
                                        egui::RichText::new("Remove rule").color(palette.danger),
                                    )
                                    .clicked()
                                {
                                    remove_rule = Some(index);
                                    ui.close();
                                }
                            });
                        });
                    });
                    rule_fields(
                        ui,
                        entity,
                        index,
                        rule,
                        owner_is_button,
                        &text_targets,
                        &audio_targets,
                    );
                });
        }
    });
    graph::show(
        ui.ctx(),
        entity,
        &mut rules,
        graph::Controls {
            text_targets: &text_targets,
            audio_targets: &audio_targets,
            owner_is_button,
            can_add_rule,
            add_rule: &mut add_rule,
            remove_rule: &mut remove_rule,
        },
    );
    if let Some(index) = remove_rule {
        rules.remove(index);
    }
    if rules != before {
        if rules.is_empty() {
            world.entity_mut(entity).remove::<SceneRules>();
        } else {
            world.entity_mut(entity).insert(SceneRules(rules));
        }
    }
    if let Some((kind, trigger)) = add_rule {
        add_visual_rule(world, entity, kind, trigger);
    }

    if world.get::<MeshSource>(entity).is_some() {
        let mut changed = false;
        let mut reset = false;
        let custom = world.get::<SceneMaterialOverride>(entity).is_some();
        section(
            ui,
            "Material",
            custom.then_some(|ui: &mut egui::Ui| {
                if ui.button("Use imported material").clicked() {
                    reset = true;
                    ui.close();
                }
            }),
            |ui| {
                if let Some(mut value) = world
                    .get::<SceneMaterialOverride>(entity)
                    .map(|current| current.0.clone())
                {
                    let mut edited = property_row(ui, "Tint", |ui| {
                        ui.color_edit_button_rgb(&mut value.tint)
                            .on_hover_text("Multiplies the imported color")
                            .changed()
                    });
                    for (label, factor) in [
                        ("Metallic", &mut value.metallic),
                        ("Roughness", &mut value.roughness),
                    ] {
                        property_row(ui, label, |ui| {
                            let mut enabled = factor.is_some();
                            if crate::widgets::checkbox(ui, &palette, &mut enabled, "")
                                .on_hover_text("Override the imported value")
                                .changed()
                            {
                                *factor = enabled.then_some(0.5);
                                edited = true;
                            }
                            if let Some(value) = factor {
                                edited |= slider(ui, value, 0.0..=1.0);
                            } else {
                                ui.label(egui::RichText::new("Imported").color(palette.subtle));
                            }
                        });
                    }
                    if edited {
                        changed = set_material_override(world, entity, Some(value));
                    }
                } else {
                    note(ui, "Using the imported material.");
                    if ui.button("Customize material").clicked() {
                        changed = set_material_override(
                            world,
                            entity,
                            Some(MaterialOverrideData::default()),
                        );
                    }
                }
            },
        );
        if reset {
            changed = set_material_override(world, entity, None);
        }
        if changed && apply_to_all {
            for &other in others {
                copy_material_override(world, entity, other);
            }
        }
    }

    ui.add_space(4.0);
    // A 3D model can't also show a flat image, so only other objects
    // offer to take one.
    if world.get::<SpriteSource>(entity).is_none()
        && world.get::<Sprite>(entity).is_none()
        && world.get::<MeshSource>(entity).is_none()
        && let Some(path) = image_drop_zone(
            ui,
            "Add an image",
            Some("Drag a PNG or JPEG here from Project Assets"),
        )
    {
        set_sprite_image(world, entity, assets, &path);
    }
    add_component_menu(ui, world, entity);
}

/// The Inspector's spacing: every control 24 points tall with the same
/// padding, so buttons, fields, drop-downs and checkboxes line up.
fn inspector_style(style: &mut egui::Style) {
    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(6.0, 6.0);
    spacing.button_padding = egui::vec2(10.0, 4.0);
    spacing.interact_size.y = 24.0;
    spacing.combo_width = 120.0;
    style.visuals.text_edit_bg_color = Some(crate::theme::NIGHT.hover);
}

/// A short muted line under a section's title or a field.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(11.5)
            .color(crate::theme::NIGHT.muted),
    );
}

/// A drop-down that fills the rest of its row, with the trees' chevron.
fn choice(id_salt: impl std::hash::Hash + std::fmt::Debug, ui: &egui::Ui) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id_salt)
        .width(ui.available_width())
        .icon(crate::widgets::combo_chevron)
}

/// A component section: a soft filled card with its name and, at the
/// right, a "⋯" menu of less-used actions. Sections don't fold: an object
/// has only a few, so every property is in view.
fn section(
    ui: &mut egui::Ui,
    title: &str,
    menu: Option<impl FnOnce(&mut egui::Ui)>,
    add_body: impl FnOnce(&mut egui::Ui),
) {
    let palette = crate::theme::NIGHT;
    component_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), SECTION_HEADER_HEIGHT),
            egui::Sense::hover(),
        );
        let room = rect.width() - if menu.is_some() { 28.0 } else { 0.0 };
        let galley = crate::theme::weighted_galley(
            ui.painter(),
            title,
            egui::FontId::proportional(12.5),
            palette.text,
            crate::theme::SEMIBOLD,
            Some(room.max(0.0)),
        );
        let at = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(at, galley, palette.text);
        if let Some(menu) = menu {
            let slot = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 11.0, rect.center().y),
                egui::Vec2::splat(22.0),
            );
            let more = ui
                .scope_builder(egui::UiBuilder::new().max_rect(slot), |ui| {
                    crate::widgets::more_button(ui, &palette)
                })
                .inner;
            egui::Popup::menu(&more).show(|ui| {
                crate::theme::style_context_menu(ui);
                menu(ui);
            });
        }
        add_body(ui);
        ui.add_space(6.0);
    });
    ui.add_space(2.0);
}

/// A small heading inside a section for a set of related properties.
fn subsection(ui: &mut egui::Ui, title: &str, add_body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .size(11.5)
            .color(crate::theme::NIGHT.subtle),
    );
    add_body(ui);
}

/// A slider that fills the rest of its row, with its value in a number
/// field one cell wide at the end.
fn slider(ui: &mut egui::Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let spacing = ui.spacing().item_spacing.x;
    let field = crate::inspect::cell_width(ui).min(64.0);
    // The knob reaches past the track's ends by its radius.
    let knob = ui.spacing().interact_size.y / 2.0;
    // In a wrapping row `available_width` is the whole row, so measure
    // what the controls before this one left.
    let left = ui.max_rect().right() - ui.cursor().min.x;
    ui.spacing_mut().slider_width = (left - spacing - field - knob).max(40.0);
    ui.visuals_mut().slider_trailing_fill = true;
    let speed = ((*range.end() - *range.start()) / 500.0).clamp(0.001, 10.0);
    let changed = ui
        .add(
            egui::Slider::new(value, range.clone())
                .show_value(false)
                .handle_shape(egui::style::HandleShape::Circle),
        )
        .changed();
    let number = ui.add_sized(
        [field, ui.spacing().interact_size.y],
        egui::DragValue::new(value)
            .range(range)
            .speed(speed)
            .max_decimals(2),
    );
    crate::inspect::scrub_cursor(ui, &number);
    changed | number.changed()
}

/// The kind of a collider's shape, in a word.
fn shape_name(shape: &ColliderShape) -> &'static str {
    match shape {
        ColliderShape::Ball { .. } => "Ball",
        ColliderShape::Cuboid { .. } => "Box",
        ColliderShape::Cylinder { .. } => "Cylinder",
        ColliderShape::Capsule { .. } => "Capsule",
        ColliderShape::TriMesh { .. } => "Mesh",
    }
}

/// The height of a section's header row.
const SECTION_HEADER_HEIGHT: f32 = 30.0;

/// A component section's card: a soft fill with no outline, full width so
/// every section in the Inspector lines up.
fn component_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(crate::theme::NIGHT.raised)
        .corner_radius(9)
        .inner_margin(egui::Margin {
            left: 10,
            right: 8,
            top: 1,
            bottom: 4,
        })
}

/// The object's identity, above its sections: its name as a larger field,
/// its id, and the Static / Disabled / Locked markers.
fn identity_header(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    let palette = crate::theme::NIGHT;
    let mut name = name_of(world, entity).unwrap_or_default();
    let field = crate::widgets::field(ui, &mut name, 14.0, 30.0)
        .hint_text("Name")
        .desired_width(ui.available_width());
    let field = ui.add(field);
    if field.changed() {
        set_name(world, entity, name);
    }
    ui.label(
        egui::RichText::new(format!("ID {entity:?}"))
            .size(11.5)
            .color(palette.subtle),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        let mut is_static = has::<Static>(world, entity);
        if crate::widgets::checkbox(ui, &palette, &mut is_static, "Static").changed() {
            toggle_marker::<Static>(world, entity, is_static);
        }
        let mut disabled = has::<Disabled>(world, entity);
        if crate::widgets::checkbox(ui, &palette, &mut disabled, "Disabled").changed() {
            toggle_marker::<Disabled>(world, entity, disabled);
        }
        let mut locked = has::<Lock>(world, entity);
        if crate::widgets::checkbox(ui, &palette, &mut locked, "Locked").changed() {
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
    let palette = crate::theme::NIGHT;
    let button = egui::Button::new(egui::RichText::new("Add component").color(palette.accent))
        .fill(palette.raised)
        .corner_radius(9)
        .min_size(egui::vec2(ui.available_width(), 30.0));
    egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        crate::theme::style_context_menu(ui);
        let mut any = false;
        for component in REGISTRY {
            if component.name == "Sprite" || (component.present)(world, entity) {
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
    fn graph_reconnects_only_the_chosen_action_to_an_existing_event() {
        let target = uuid::Uuid::new_v4().to_string();
        let mut rules = vec![
            RuleData {
                trigger: RuleTrigger::Named("level_start".into()),
                action: RuleAction::SetUiText {
                    target: target.clone(),
                    text: "Ready".into(),
                },
                condition: None,
                graph_position: None,
            },
            RuleData {
                trigger: RuleTrigger::Named("score_changed".into()),
                action: RuleAction::SetUiText {
                    target,
                    text: "Score".into(),
                },
                condition: None,
                graph_position: None,
            },
        ];
        assert_eq!(graph::event_nodes(&rules).len(), 2);
        let event = rules[0].trigger.clone();
        assert!(graph::connect(&mut rules, &event, 1));
        assert_eq!(rules[1].trigger, RuleTrigger::Named("level_start".into()));
        assert_eq!(graph::event_nodes(&rules).len(), 1);
        assert!(
            matches!(rules[1].action, RuleAction::SetUiText { ref text, .. } if text == "Score")
        );
        assert!(!graph::connect(
            &mut rules,
            &RuleTrigger::Named("deleted_event".into()),
            1,
        ));
        assert!(!graph::connect(&mut rules, &event, 4));
    }

    #[test]
    fn graph_added_action_uses_the_selected_event() {
        let mut world = World::new();
        let owner = world.spawn(Name::new("Rule owner")).id();
        let event = RuleTrigger::Named("level_start".into());
        assert!(add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Event,
            Some(event.clone())
        ));
        let rules = &world.get::<SceneRules>(owner).expect("saved rules").0;
        assert_eq!(rules[0].trigger, event);
    }

    #[test]
    fn graph_rejects_a_button_event_on_a_non_button_without_assigning_ids() {
        let mut world = World::new();
        let owner = world.spawn(Name::new("Rule owner")).id();
        assert!(!add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Event,
            Some(RuleTrigger::UiClicked)
        ));
        assert!(world.get::<SceneObjectId>(owner).is_none());
        assert!(world.get::<SceneRules>(owner).is_none());
    }

    #[test]
    fn visual_rule_uses_stable_named_target_after_scene_reopen() {
        let mut world = World::new();
        let button = world
            .spawn((Name::new("Start"), SceneUi(UiElementData::button())))
            .id();
        let label = world
            .spawn((Name::new("Status"), SceneUi(UiElementData::label())))
            .id();

        assert!(add_visual_rule(
            &mut world,
            button,
            RuleActionKind::Text,
            None
        ));
        let button_id = world.get::<SceneObjectId>(button).unwrap().0.clone();
        let label_id = world.get::<SceneObjectId>(label).unwrap().0.clone();
        assert!(
            rule_targets(&world, RuleActionKind::Text)
                .contains(&(label_id.clone(), "Status".into()))
        );
        world.get_mut::<SceneRules>(button).unwrap().0[0].action = RuleAction::SetUiText {
            target: label_id.clone(),
            text: "Ready".into(),
        };
        assert!(matches!(
            &world.get::<SceneRules>(button).unwrap().0[0],
            RuleData {
                trigger: RuleTrigger::UiClicked,
                action: RuleAction::SetUiText { target, .. },
                condition: None,
                graph_position: None,
            } if target == &label_id
        ));

        let saved = engine_scene::Scene::from_world(&mut world)
            .to_ron_string()
            .unwrap();
        let reopened = engine_scene::Scene::from_ron_str(&saved).unwrap();
        reopened.validate().unwrap();
        let saved_button = reopened
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some("Start"))
            .unwrap();
        assert_eq!(saved_button.object_id.as_deref(), Some(button_id.as_str()));
        assert_eq!(saved_button.rules.len(), 1);
        assert!(matches!(
            &saved_button.rules[0].action,
            RuleAction::SetUiText { target, text } if target == &label_id && text == "Ready"
        ));
    }

    #[test]
    fn visual_rule_addition_stops_at_scene_limit() {
        let mut world = World::new();
        let owner = world
            .spawn((Name::new("Start"), SceneUi(UiElementData::button())))
            .id();
        let target = uuid::Uuid::new_v4().to_string();
        world.spawn((
            Name::new("Status"),
            SceneObjectId(target.clone()),
            SceneUi(UiElementData::label()),
        ));
        let rule = RuleData {
            trigger: RuleTrigger::Named("start".into()),
            action: RuleAction::SetUiText {
                target,
                text: "Ready".into(),
            },
            condition: None,
            graph_position: None,
        };
        for _ in 0..64 {
            world.spawn((Name::new("Rules"), SceneRules(vec![rule.clone(); 16])));
        }

        assert!(!add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Text,
            None
        ));
        assert!(world.get::<SceneRules>(owner).is_none());
    }

    #[test]
    fn visual_variable_action_adds_a_score_counter_without_a_target() {
        let mut world = World::new();
        let owner = world.spawn(Name::new("Coin")).id();
        assert!(add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Variable,
            Some(RuleTrigger::Named("coin".into()))
        ));
        let rules = &world.get::<SceneRules>(owner).expect("authored rules").0;
        assert_eq!(
            rules[0],
            RuleData::new(
                RuleTrigger::Named("coin".into()),
                RuleAction::AddToVariable {
                    name: "score".into(),
                    amount: 1.0,
                },
            )
        );
    }

    #[test]
    fn visual_event_action_can_be_added_without_a_target_object() {
        let mut world = World::new();
        let owner = world.spawn(Name::new("Rules")).id();
        assert!(add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Event,
            None
        ));
        let rules = &world.get::<SceneRules>(owner).expect("authored rules").0;
        assert!(matches!(
            &rules[0],
            RuleData {
                trigger: RuleTrigger::Named(trigger),
                action: RuleAction::EmitEvent { name },
                condition: None,
                graph_position: None,
            } if trigger == "game_start" && name == "next_event"
        ));
    }

    #[test]
    fn material_override_only_edits_meshes_and_can_copy_or_reset() {
        let mut world = World::new();
        let source = engine_ecs::components::MeshSource::new("mesh-id", "material-id");
        let from = world.spawn(source.clone()).id();
        let to = world.spawn(source).id();
        let non_mesh = world.spawn_empty().id();
        let red = engine_scene::MaterialOverrideData {
            tint: [1.0, 0.3, 0.3],
            metallic: Some(0.25),
            roughness: Some(0.8),
        };

        assert!(!set_material_override(
            &mut world,
            non_mesh,
            Some(red.clone())
        ));
        assert!(
            world
                .get::<engine_scene::SceneMaterialOverride>(non_mesh)
                .is_none()
        );
        assert!(set_material_override(&mut world, from, Some(red.clone())));
        copy_material_override(&mut world, from, to);
        assert_eq!(
            world
                .get::<engine_scene::SceneMaterialOverride>(to)
                .unwrap()
                .0,
            red
        );

        assert!(set_material_override(&mut world, from, None));
        copy_material_override(&mut world, from, to);
        assert!(
            world
                .get::<engine_scene::SceneMaterialOverride>(to)
                .is_none()
        );
        assert!(!set_material_override(&mut world, from, None));
    }

    #[test]
    fn ui_component_can_be_added_and_batch_copied() {
        let mut world = World::new();
        let from = world.spawn_empty().id();
        let to = world.spawn_empty().id();
        let ui = REGISTRY
            .iter()
            .find(|component| component.name == "Game UI")
            .expect("Game UI registry entry");
        (ui.insert_default)(&mut world, from);
        (ui.insert_default)(&mut world, to);
        let mut value = world.get::<engine_scene::SceneUi>(from).unwrap().0.clone();
        value.text = "Score: 0".into();
        world
            .entity_mut(from)
            .insert(engine_scene::SceneUi(value.clone()));

        (ui.copy_to)(&mut world, from, to);

        assert_eq!(world.get::<engine_scene::SceneUi>(to).unwrap().0, value);
        (ui.remove)(&mut world, to);
        assert!(world.get::<engine_scene::SceneUi>(to).is_none());
    }

    #[test]
    fn sprite_collision_fits_image_and_survives_scene_reopen() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name::new("Hero"),
                Transform::default(),
                SpriteSource {
                    atlas: uuid::Uuid::nil().to_string(),
                    region: "full".into(),
                    size: [4.0, 2.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
            ))
            .id();

        assert!(fit_sprite_collider(&mut world, entity));
        let collider = world.get::<SceneCollider>(entity).unwrap().0.clone();
        assert_eq!(collider.body, BodyKind::Fixed);
        assert_eq!(
            collider.shape,
            ColliderShape::Cuboid {
                half_extents: [2.0, 1.0, 0.05]
            }
        );
        let ron = engine_scene::Scene::from_world(&mut world)
            .to_ron_string()
            .unwrap();
        let saved = engine_scene::Scene::from_ron_str(&ron).unwrap();
        assert_eq!(saved.entities[0].collider.as_ref(), Some(&collider));
    }

    #[test]
    fn sprite_collision_refit_preserves_body_and_rejects_invalid_size() {
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: uuid::Uuid::nil().to_string(),
                    region: "full".into(),
                    size: [2.0, 3.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                SceneCollider(engine_scene::ColliderData {
                    shape: ColliderShape::Ball { radius: 1.0 },
                    body: BodyKind::Dynamic,
                }),
            ))
            .id();

        assert!(fit_sprite_collider(&mut world, entity));
        let collider = &world.get::<SceneCollider>(entity).unwrap().0;
        assert_eq!(collider.body, BodyKind::Dynamic);
        assert_eq!(
            collider.shape,
            ColliderShape::Cuboid {
                half_extents: [1.0, 1.5, 0.05]
            }
        );
        assert!(!fit_sprite_collider(&mut world, entity));
        world.get_mut::<SpriteSource>(entity).unwrap().size = [f32::NAN, 2.0];
        assert!(!fit_sprite_collider(&mut world, entity));
        assert!(world.get::<SceneCollider>(entity).is_some());
    }

    #[test]
    fn changing_sprite_image_preserves_properties_and_updates_saved_reference() {
        use engine_ecs::components::SpriteSource;

        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut assets = AssetIndex::new();
        let old = std::path::Path::new("sprites/old.png");
        let new = std::path::Path::new("sprites/new.png");
        let old_id = engine_asset::AssetId::new();
        let new_id = engine_asset::AssetId::new();
        assets.insert(old_id, old.to_path_buf());
        assets.insert(new_id, new.to_path_buf());
        assert!(set_sprite_image(&mut world, entity, &assets, old));
        world.entity_mut(entity).insert(AssetSource::with_id(
            old.to_string_lossy().into_owned(),
            old_id.to_string(),
        ));
        let mut sprite = *world.get::<Sprite>(entity).unwrap();
        sprite.size = Vec2::new(2.0, 3.0);
        sprite.color = [0.5, 0.75, 1.0, 1.0];
        sprite.z_order = 4.0;
        world.entity_mut(entity).insert(sprite);
        world.get_mut::<SpriteSource>(entity).unwrap().region = "grid:2,2:1,0".into();

        assert!(set_sprite_image(&mut world, entity, &assets, new));

        let source = world.get::<SpriteSource>(entity).unwrap();
        assert_eq!(source.atlas, new_id.to_string());
        assert_eq!(source.region, "grid:2,2:1,0");
        let asset_source = world.get::<AssetSource>(entity).unwrap();
        assert_eq!(asset_source.path, new.to_string_lossy());
        assert_eq!(
            asset_source.id.as_deref(),
            Some(new_id.to_string().as_str())
        );
        let saved = engine_scene::capture_renderables(&world, entity).1.unwrap();
        assert_eq!(saved.atlas.id, new_id.to_string());
        assert_eq!(saved.region, "grid:2,2:1,0");
        let sprite = world.get::<Sprite>(entity).unwrap();
        assert_eq!(sprite.size, Vec2::new(2.0, 3.0));
        assert_eq!(sprite.color, [0.5, 0.75, 1.0, 1.0]);
        assert_eq!(sprite.z_order, 4.0);
    }

    #[test]
    fn sprite_image_drop_rejects_unindexed_and_non_image_assets() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut assets = AssetIndex::new();
        let mesh = std::path::Path::new("props/rock.glb");
        assets.insert(engine_asset::AssetId::new(), mesh.to_path_buf());
        assert!(!set_sprite_image(
            &mut world,
            entity,
            &assets,
            std::path::Path::new("sprites/missing.png")
        ));
        assert!(!set_sprite_image(&mut world, entity, &assets, mesh));
        assert!(world.get::<Sprite>(entity).is_none());
    }

    #[test]
    fn removing_and_batch_copying_sprite_keeps_source_in_sync() {
        use engine_ecs::components::SpriteSource;

        let mut world = World::new();
        let from = world.spawn_empty().id();
        let to = world.spawn_empty().id();
        let mut assets = AssetIndex::new();
        let first = std::path::Path::new("sprites/first.png");
        let second = std::path::Path::new("sprites/second.png");
        let first_id = engine_asset::AssetId::new();
        let second_id = engine_asset::AssetId::new();
        assets.insert(first_id, first.to_path_buf());
        assets.insert(second_id, second.to_path_buf());
        assert!(set_sprite_image(&mut world, from, &assets, first));
        assert!(set_sprite_image(&mut world, to, &assets, second));
        world.get_mut::<SpriteSource>(from).unwrap().region = "grid:2,1:0,0".into();
        world
            .entity_mut(from)
            .insert(SceneSpriteAnimation::new(SpriteAnimationData {
                frames: 2,
                fps: 8.0,
                looping: true,
            }));
        world.entity_mut(from).insert(AssetSource::with_id(
            first.to_string_lossy().into_owned(),
            first_id.to_string(),
        ));
        world.entity_mut(to).insert(AssetSource::with_id(
            second.to_string_lossy().into_owned(),
            second_id.to_string(),
        ));

        copy_sprite(&mut world, from, to);
        assert_eq!(
            engine_scene::capture_renderables(&world, from).1,
            engine_scene::capture_renderables(&world, to).1
        );
        assert_eq!(world.get::<AssetSource>(to), world.get::<AssetSource>(from));
        assert!(world.get::<SceneSpriteAnimation>(to).is_some());
        remove_sprite(&mut world, to);
        assert!(world.get::<Sprite>(to).is_none());
        assert!(world.get::<SpriteSource>(to).is_none());
        assert!(world.get::<SceneSpriteAnimation>(to).is_none());
        assert!(world.get::<AssetSource>(to).is_none());
        assert!(engine_scene::capture_renderables(&world, to).1.is_none());

        assert!(set_sprite_image(&mut world, to, &assets, second));
        world.entity_mut(from).remove::<Sprite>();
        copy_sprite(&mut world, from, to);
        assert!(world.get::<Sprite>(to).is_none());
        assert_eq!(
            world.get::<SpriteSource>(to).unwrap().atlas,
            world.get::<SpriteSource>(from).unwrap().atlas
        );
    }

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
    fn water_component_can_be_added_with_valid_authoring_defaults() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let water_ui = REGISTRY
            .iter()
            .find(|component| component.name == "Water")
            .expect("a Water registry entry");
        (water_ui.insert_default)(&mut world, entity);

        let volume = world
            .get::<engine_ecs::components::WaterVolume>(entity)
            .expect("water component");
        assert!(volume.surface.half_extent.min_element() > 0.0);
        assert!(volume.surface.sample(Vec3::ZERO, 0.0).is_some());
    }

    #[test]
    fn point_light_component_can_be_added_to_a_scene_object() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let light_ui = REGISTRY
            .iter()
            .find(|component| component.name == "Point Light")
            .expect("point light registry entry");
        (light_ui.insert_default)(&mut world, entity);
        let light = world
            .get::<engine_scene::ScenePointLight>(entity)
            .expect("point light component");
        assert!(light.0.intensity > 0.0);
        assert!(light.0.range > 0.0);
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
                &AssetIndex::new(),
                &mut open_script,
            );
        });
        // No interaction in a headless pass, so nothing is requested.
        assert_eq!(open_script, None);
    }

    #[test]
    fn script_paths_accept_project_relative_rust_or_lua_files() {
        assert!(valid_script_path(std::path::Path::new("src/player.rs")));
        assert!(valid_script_path(std::path::Path::new(
            "assets/scripts/player.lua"
        )));
        assert!(!valid_script_path(std::path::Path::new("src/player.ron")));
        assert!(!valid_script_path(std::path::Path::new("src/player.lua")));
        assert!(!valid_script_path(std::path::Path::new(
            "assets/scripts/player.rs"
        )));
        assert!(!valid_script_path(std::path::Path::new("../player.rs")));
        assert!(!valid_script_path(std::path::Path::new("/tmp/player.rs")));
    }

    #[test]
    fn typed_asset_targets_reject_mismatched_kinds() {
        assert!(accepts_asset_kind(AssetKind::Audio, AssetKind::Audio));
        assert!(!accepts_asset_kind(AssetKind::Texture, AssetKind::Audio));
        assert!(!accepts_asset_kind(AssetKind::Other, AssetKind::Mesh));
    }

    #[test]
    fn audio_drop_assigns_indexed_id_without_accepting_a_missing_or_wrong_asset() {
        let mut index = crate::assets::AssetIndex::new();
        let id = engine_utils::AssetId::new();
        index.insert(id, std::path::PathBuf::from("audio/footstep.wav"));
        let mut emitter = AudioEmitterData::new(AssetRef { id: String::new() });

        assert!(set_emitter_sound_from_drop(
            &mut emitter,
            &index,
            std::path::Path::new("audio/footstep.wav")
        ));
        assert_eq!(emitter.sound.id, id.to_string());
        assert!(!set_emitter_sound_from_drop(
            &mut emitter,
            &index,
            std::path::Path::new("audio/missing.wav")
        ));
        assert!(!set_emitter_sound_from_drop(
            &mut emitter,
            &index,
            std::path::Path::new("textures/picture.png")
        ));
        assert_eq!(emitter.sound.id, id.to_string());
    }

    #[test]
    fn collider_summary_covers_dimensions_without_live_physics_handles() {
        let shape = ColliderShape::Cuboid {
            half_extents: [1.0, 2.0, 3.0],
        };
        assert_eq!(
            collider_shape_summary(&shape),
            "Cuboid (half-extents [1.000, 2.000, 3.000])"
        );
    }
}
