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
use crate::inspect::{InspectField, Inspectable};

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
    egui::ComboBox::from_id_salt(("visual_rule_target", salt))
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
    ui.horizontal(|ui| {
        ui.label("When");
        egui::ComboBox::from_id_salt(("visual_rule_trigger", entity, index))
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
        ui.add(egui::TextEdit::singleline(name).hint_text("Event name"));
    }
    ui.horizontal(|ui| {
        ui.label("Do");
        match &mut rule.action {
            RuleAction::SetUiText { target, .. } => {
                ui.label("Set UI text on");
                rule_target_picker(ui, (entity, index), target, text_targets);
            }
            RuleAction::PlayAudio { target } => {
                ui.label("Play sound on");
                rule_target_picker(ui, (entity, index), target, audio_targets);
            }
            RuleAction::EmitEvent { name } => {
                ui.label("Emit event");
                ui.add(egui::TextEdit::singleline(name).hint_text("Event name"));
            }
            RuleAction::SetVariable { .. } | RuleAction::AddToVariable { .. } => {
                variable_action_fields(ui, (entity, index), &mut rule.action);
            }
        }
    });
    if let RuleAction::SetUiText { text, .. } = &mut rule.action {
        ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
        ui.weak("Write {score} to show a variable's value.");
    }
    rule_condition_fields(ui, (entity, index), &mut rule.condition);
}

fn variable_action_fields(ui: &mut egui::Ui, salt: (Entity, usize), action: &mut RuleAction) {
    let adding = matches!(action, RuleAction::AddToVariable { .. });
    egui::ComboBox::from_id_salt(("visual_rule_variable_op", salt))
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
    let (name, number, preposition) = match action {
        RuleAction::SetVariable { name, value } => (name, value, "to"),
        RuleAction::AddToVariable { name, amount } => (name, amount, "by"),
        _ => return,
    };
    ui.add(
        egui::TextEdit::singleline(name)
            .hint_text("Variable")
            .desired_width(90.0),
    );
    ui.label(preposition);
    ui.add(egui::DragValue::new(number).speed(0.1));
}

fn rule_condition_fields(
    ui: &mut egui::Ui,
    salt: (Entity, usize),
    condition: &mut Option<RuleCondition>,
) {
    ui.horizontal(|ui| {
        let mut enabled = condition.is_some();
        if ui.checkbox(&mut enabled, "Only if").changed() {
            *condition = enabled.then(|| RuleCondition {
                variable: "score".into(),
                compare: RuleCompare::GreaterOrEqual,
                value: 1.0,
            });
        }
        let Some(condition) = condition else {
            return;
        };
        ui.add(
            egui::TextEdit::singleline(&mut condition.variable)
                .hint_text("Variable")
                .desired_width(90.0),
        );
        egui::ComboBox::from_id_salt(("visual_rule_compare", salt))
            .width(44.0)
            .selected_text(condition.compare.symbol())
            .show_ui(ui, |ui| {
                for compare in RuleCompare::ALL {
                    ui.selectable_value(&mut condition.compare, compare, compare.symbol());
                }
            });
        ui.add(egui::DragValue::new(&mut condition.value).speed(0.1));
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
            let mut changed = ui.color_edit_button_rgb(&mut data.color).changed();
            changed |= ui
                .add(egui::Slider::new(&mut data.intensity, 0.0..=1000.0).text("Intensity"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut data.range, 0.01..=10_000.0).text("Guide range"))
                .changed();
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
            ui.label("Position and size");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut data.center[0])
                        .prefix("X ")
                        .speed(0.1),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut data.center[2])
                        .prefix("Z ")
                        .speed(0.1),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut data.base_height)
                        .prefix("Level ")
                        .speed(0.1),
                )
                .changed();
            for (label, value) in ["Half width", "Half depth"]
                .into_iter()
                .zip(&mut data.half_extent)
            {
                changed |= ui
                    .add(
                        egui::DragValue::new(value)
                            .prefix(format!("{label} "))
                            .speed(0.1)
                            .range(0.01..=10000.0),
                    )
                    .changed();
            }
            ui.collapsing("Waves and physics", |ui| {
                for (label, value) in ["Direction X", "Direction Z"]
                    .into_iter()
                    .zip(&mut data.wave_direction)
                {
                    changed |= ui
                        .add(
                            egui::DragValue::new(value)
                                .prefix(format!("{label} "))
                                .speed(0.05)
                                .range(-1.0..=1.0),
                        )
                        .changed();
                }
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut data.wave_amplitude)
                            .prefix("Amplitude ")
                            .speed(0.01)
                            .range(0.0..=100.0),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut data.wave_length)
                            .prefix("Length ")
                            .speed(0.1)
                            .range(0.01..=10000.0),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut data.wave_speed)
                            .prefix("Speed ")
                            .speed(0.05),
                    )
                    .changed();
                for (label, value) in ["Current X", "Current Y", "Current Z"]
                    .into_iter()
                    .zip(&mut data.current)
                {
                    changed |= ui
                        .add(
                            egui::DragValue::new(value)
                                .prefix(format!("{label} "))
                                .speed(0.05),
                        )
                        .changed();
                }
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut data.density)
                            .prefix("Density ")
                            .speed(10.0)
                            .range(0.01..=100000.0),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut data.linear_drag)
                            .prefix("Drag ")
                            .speed(0.05)
                            .range(0.0..=1000.0),
                    )
                    .changed();
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
                    ui.colored_label(egui::Color32::RED, error.to_string());
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
            ui.horizontal(|ui| {
                ui.label("Sound");
                changed |= value.sound.id.inspect_field(ui, 0.0);
                if let Some(dropped) = released_asset(ui, AssetKind::Audio) {
                    if set_emitter_sound_from_drop(&mut value, assets, &dropped) {
                        changed = true;
                    } else {
                        ui.weak("Import this audio file before assigning it.");
                    }
                }
            });
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut value.autoplay, "Autoplay").changed();
                changed |= ui.checkbox(&mut value.looping, "Loop").changed();
            });
            ui.horizontal(|ui| {
                ui.label("Gain");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut value.gain)
                            .speed(0.05)
                            .range(0.0..=4.0),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("Radius");
                changed |= ui
                    .add(
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
            let fit_sprite = if world.get::<SpriteSource>(entity).is_some()
                || world.get::<Sprite>(entity).is_some()
            {
                ui.small_button("Fit box to sprite").clicked()
            } else {
                false
            };
            egui::ComboBox::from_label("Body")
                .selected_text(format!("{:?}", value.0.body))
                .show_ui(ui, |ui| {
                    for body in [BodyKind::Fixed, BodyKind::Dynamic, BodyKind::Kinematic] {
                        changed |= ui
                            .selectable_value(&mut value.0.body, body, format!("{body:?}"))
                            .changed();
                    }
                });
            ui.label(collider_shape_summary(&value.0.shape));
            match &mut value.0.shape {
                ColliderShape::Ball { radius } => {
                    changed |= ui
                        .add(
                            egui::DragValue::new(radius)
                                .prefix("Radius ")
                                .speed(0.05)
                                .range(0.001..=10000.0),
                        )
                        .changed()
                }
                ColliderShape::Cuboid { half_extents } => {
                    for (axis, extent) in ["X", "Y", "Z"].into_iter().zip(half_extents) {
                        changed |= ui
                            .add(
                                egui::DragValue::new(extent)
                                    .prefix(format!("Half {axis} "))
                                    .speed(0.05)
                                    .range(0.001..=10000.0),
                            )
                            .changed();
                    }
                }
                ColliderShape::Cylinder {
                    half_height,
                    radius,
                }
                | ColliderShape::Capsule {
                    half_height,
                    radius,
                } => {
                    changed |= ui
                        .add(
                            egui::DragValue::new(half_height)
                                .prefix("Half height ")
                                .speed(0.05)
                                .range(0.001..=10000.0),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(radius)
                                .prefix("Radius ")
                                .speed(0.05)
                                .range(0.001..=10000.0),
                        )
                        .changed();
                }
                ColliderShape::TriMesh { mesh } => {
                    ui.label(format!("Mesh asset {}", mesh.id));
                    if released_asset(ui, AssetKind::Mesh).is_some() {
                        ui.weak("Mesh drop received; import resolution will assign its stable id.");
                    }
                }
            }
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
                let toggled = ui
                    .checkbox(&mut use_grid, "Use sprite-sheet cell")
                    .on_hover_text(
                        "Divide the image into equal columns and rows, then choose one cell",
                    )
                    .changed();
                let region = if use_grid {
                    let mut grid = parsed.unwrap_or(GridRegion {
                        columns: 2,
                        rows: 2,
                        column: 0,
                        row: 0,
                    });
                    ui.horizontal(|ui| {
                        ui.label("Grid");
                        ui.add(
                            egui::DragValue::new(&mut grid.columns)
                                .range(1..=4096)
                                .prefix("Columns "),
                        );
                        ui.add(
                            egui::DragValue::new(&mut grid.rows)
                                .range(1..=4096)
                                .prefix("Rows "),
                        );
                    });
                    grid.column = grid.column.min(grid.columns.saturating_sub(1));
                    grid.row = grid.row.min(grid.rows.saturating_sub(1));
                    ui.horizontal(|ui| {
                        ui.label("Cell");
                        ui.add(
                            egui::DragValue::new(&mut grid.column)
                                .range(0..=grid.columns - 1)
                                .prefix("X "),
                        );
                        ui.add(
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
                    ui.checkbox(&mut enabled, "Animate sheet").on_hover_text(
                        "Play consecutive cells from the selected cell in Play mode",
                    );
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
                        ui.horizontal(|ui| {
                            ui.label("Frames");
                            ui.add(egui::DragValue::new(&mut data.frames).range(1..=available));
                            ui.label("FPS");
                            ui.add(
                                egui::DragValue::new(&mut data.fps)
                                    .speed(0.1)
                                    .range(0.1..=120.0),
                            );
                        });
                        ui.checkbox(&mut data.looping, "Loop");
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
                && ui.small_button("Add box collision").clicked()
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

            let changed = ui
                .horizontal(|ui| {
                    ui.label("Path");
                    path.inspect_field(ui, 0.0)
                })
                .inner;
            let mut changed = changed;
            if let Some(dropped) = released_asset(ui, AssetKind::Mesh) {
                path = dropped.to_string_lossy().into_owned();
                changed = true;
            }

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
            egui::ComboBox::from_label("Widget")
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
            ui.horizontal(|ui| {
                ui.label("Text");
                changed |= ui
                    .add(egui::TextEdit::singleline(&mut data.text).char_limit(1024))
                    .changed();
            });
            egui::ComboBox::from_label("Anchor")
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
            for (label, values) in [("Offset", &mut data.offset), ("Size", &mut data.size)] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    for value in values {
                        let mut control = egui::DragValue::new(value).speed(1.0);
                        if label == "Size" {
                            control = control.range(1.0..=10000.0);
                        }
                        changed |= ui.add(control).changed();
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(if data.kind == UiKind::Label {
                    "Text color"
                } else {
                    "Background"
                });
                changed |= ui
                    .color_edit_button_rgba_unmultiplied(&mut data.color)
                    .changed();
            });
            if data.kind == UiKind::Button
                && let Some(id) = world.get::<engine_scene::SceneObjectId>(entity)
            {
                ui.horizontal(|ui| {
                    ui.label("Click ID");
                    ui.add(egui::Label::new(id.0.as_str()).truncate());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(id.0.clone());
                    }
                });
            }
            if data.text.len() > 1024 {
                ui.colored_label(
                    egui::Color32::RED,
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
            let changed = path.inspect_field(ui, 0.0);
            if !path.is_empty() && !valid_script_path(std::path::Path::new(&path)) {
                ui.colored_label(
                    egui::Color32::RED,
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
    ui.horizontal(|ui| {
        ui.label("Image");
        let response = ui.add_sized(
            [ui.available_width(), 26.0],
            egui::Button::new(name).sense(egui::Sense::hover()),
        );
        response
            .dnd_release_payload::<std::path::PathBuf>()
            .filter(|path| AssetKind::from_path(path) == AssetKind::Texture)
            .map(|path| (*path).clone())
    })
    .inner
}

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
    assets: &AssetIndex,
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
        component_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(
                egui::RichText::new(component.name).color(crate::theme::NIGHT.text),
            )
            .id_salt(component.name)
            .default_open(true)
            .show(ui, |ui| {
                changed = (component.inspect)(world, entity, ui, assets);
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
        });
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
        component_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(
                egui::RichText::new("Tilemap").color(crate::theme::NIGHT.text),
            )
            .id_salt("Tilemap")
            .default_open(true)
            .show(ui, |ui| {
                let image = crate::assets::parse_asset_id(&data.atlas.id)
                    .and_then(|id| assets.path_for(id))
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("Missing image");
                ui.label(format!("Image: {image}"));
                ui.weak("Paint or erase from Viewport → Tilemap in 2D mode.");
                ui.horizontal(|ui| {
                    ui.label("Sheet grid");
                    ui.add(
                        egui::DragValue::new(&mut data.columns)
                            .range(1..=4096)
                            .prefix("Columns "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut data.rows)
                            .range(1..=4096)
                            .prefix("Rows "),
                    );
                });
                data.brush_column = data.brush_column.min(data.columns - 1);
                data.brush_row = data.brush_row.min(data.rows - 1);
                ui.horizontal(|ui| {
                    ui.label("Tile size");
                    ui.add(
                        egui::DragValue::new(&mut data.cell_size[0])
                            .range(0.01..=10_000.0)
                            .prefix("W "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut data.cell_size[1])
                            .range(0.01..=10_000.0)
                            .prefix("H "),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Brush cell");
                    ui.add(
                        egui::DragValue::new(&mut data.brush_column)
                            .range(0..=data.columns - 1)
                            .prefix("X "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut data.brush_row)
                            .range(0..=data.rows - 1)
                            .prefix("Y "),
                    );
                });
                ui.checkbox(&mut data.brush_collision, "Paint solid tiles")
                    .on_hover_text(
                        "Add fixed collision to painted cells; repaint with this off to clear it",
                    );
                if let Err(error) = crate::tilemap::update_settings(world, entity, data.clone()) {
                    ui.colored_label(egui::Color32::RED, error);
                }
            });
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
    component_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::CollapsingHeader::new(
            egui::RichText::new("Visual Rules").color(crate::theme::NIGHT.text),
        )
        .id_salt("Visual Rules")
        .default_open(!rules.is_empty())
        .show(ui, |ui| {
            ui.weak("React to this button or to a named gameplay event.");
            if ui.button("Open Gameplay Graph").clicked() {
                graph::open(ui.ctx(), entity);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_add_rule && has_text_target,
                        egui::Button::new("+ Text action"),
                    )
                    .clicked()
                {
                    add_rule = Some((RuleActionKind::Text, None));
                }
                if ui
                    .add_enabled(
                        can_add_rule && has_audio_target,
                        egui::Button::new("+ Play sound"),
                    )
                    .clicked()
                {
                    add_rule = Some((RuleActionKind::Audio, None));
                }
                if ui
                    .add_enabled(can_add_rule, egui::Button::new("+ Emit event"))
                    .clicked()
                {
                    add_rule = Some((RuleActionKind::Event, None));
                }
                if ui
                    .add_enabled(can_add_rule, egui::Button::new("+ Variable"))
                    .clicked()
                {
                    add_rule = Some((RuleActionKind::Variable, None));
                }
            });
            for (index, rule) in rules.iter_mut().enumerate() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(format!("Rule {}", index + 1));
                    if ui.small_button("Remove").clicked() {
                        remove_rule = Some(index);
                    }
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
            }
        });
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
        component_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(
                egui::RichText::new("Material").color(crate::theme::NIGHT.text),
            )
            .id_salt("Mesh material")
            .default_open(true)
            .show(ui, |ui| {
                if let Some(mut value) = world
                    .get::<SceneMaterialOverride>(entity)
                    .map(|current| current.0.clone())
                {
                    let mut edited = ui
                        .horizontal(|ui| {
                            ui.label("Tint");
                            ui.color_edit_button_rgb(&mut value.tint).changed()
                        })
                        .inner;
                    ui.weak("Multiplies the imported color.");
                    for (label, factor) in [
                        ("Metallic", &mut value.metallic),
                        ("Roughness", &mut value.roughness),
                    ] {
                        ui.horizontal(|ui| {
                            let mut enabled = factor.is_some();
                            if ui.checkbox(&mut enabled, label).changed() {
                                *factor = enabled.then_some(0.5);
                                edited = true;
                            }
                            if let Some(value) = factor {
                                edited |= ui.add(egui::Slider::new(value, 0.0..=1.0)).changed();
                            }
                        });
                    }
                    if ui.button("Use imported material").clicked() {
                        changed = set_material_override(world, entity, None);
                    } else if edited {
                        changed = set_material_override(world, entity, Some(value));
                    }
                } else {
                    ui.weak("Using the imported material.");
                    if ui.button("Customize material").clicked() {
                        changed = set_material_override(
                            world,
                            entity,
                            Some(MaterialOverrideData::default()),
                        );
                    }
                }
            });
        });
        if changed && apply_to_all {
            for &other in others {
                copy_material_override(world, entity, other);
            }
        }
    }

    ui.separator();
    if world.get::<SpriteSource>(entity).is_none()
        && world.get::<Sprite>(entity).is_none()
        && let Some(path) = sprite_image_slot(ui, "Drop image here to add Sprite")
    {
        set_sprite_image(world, entity, assets, &path);
    }
    add_component_menu(ui, world, entity);
}

/// A component section: a soft filled card (no outline), full width so
/// every section in the Inspector lines up.
fn component_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(crate::theme::NIGHT.raised)
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(10, 4))
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
