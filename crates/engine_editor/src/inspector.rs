//! Inspector panel: an entity-identity header plus one editable section
//! per component the selected entity carries.
//!
//! Component types the panel knows how to show, add, and remove live in
//! a `const` table of function pointers (`REGISTRY`), so a new type is
//! one entry. The identity header shows the entity id, an editable
//! [`Name`], and the [`Disabled`] marker. Get/set helpers
//! ([`name_of`], [`set_name`], [`transform_of`], [`set_transform`]) stay
//! split out from the egui drawing so they're unit-testable without a
//! live UI — the same separation [`crate::hierarchy`] uses; the Scene
//! View gizmo also calls [`transform_of`]/[`set_transform`] directly.

use engine_ecs::components::{
    ActiveCamera, AssetSource, Camera, CameraFollow, Disabled, Lock, MeshSource, Name, Script,
    Sprite, SpriteSource, Transform, WaterVolume,
};
use engine_ecs::prelude::{Component, Entity, With, World};
use engine_utils::Transform as MathTransform;
use glam::{Vec2, Vec3};

use engine_scene::{
    AssetRef, AudioEmitterData, BodyKind, ColliderShape, GridRegion, MaterialOverrideData,
    PointLightData, RuleAction, RuleCompare, RuleCondition, RuleData, RuleTrigger,
    SceneAudioEmitter, SceneCollider, SceneMaterialOverride, SceneObjectId, ScenePointLight,
    SceneRules, SceneSpriteAnimation, SceneTilemap, SceneUi, SpawnAt, SpriteAnimationData,
    UiAnchor, UiElementData, UiKind, WaterData,
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
/// A new camera: eye at the origin looking down -Z, so the object's Transform
/// alone decides the view.
pub(crate) fn default_camera() -> Camera {
    Camera::from(engine_renderer::Camera::new(
        Vec3::ZERO,
        Vec3::NEG_Z,
        16.0 / 9.0,
    ))
}

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
    ChangeScene,
    LoadScene,
    UnloadScene,
    NextScene,
    Visible,
    Enabled,
    Spawn,
    Destroy,
    State,
}

impl RuleActionKind {
    /// Whether the action needs a named object to point at.
    fn needs_object(self) -> bool {
        matches!(self, Self::Visible | Self::Enabled | Self::Destroy)
    }
}

fn rule_target_matches(world: &World, entity: Entity, kind: RuleActionKind) -> bool {
    // Object pickers list disabled objects too, so a disabled door can be
    // enabled again; UI text and audio targets stay limited to active ones.
    world.get::<Name>(entity).is_some()
        && match kind {
            RuleActionKind::Text => {
                world.get::<Disabled>(entity).is_none()
                    && world
                        .get::<SceneUi>(entity)
                        .is_some_and(|element| element.0.kind.has_text())
            }
            RuleActionKind::Audio => {
                world.get::<Disabled>(entity).is_none()
                    && world.get::<SceneAudioEmitter>(entity).is_some()
            }
            RuleActionKind::Visible
            | RuleActionKind::Enabled
            | RuleActionKind::Destroy
            | RuleActionKind::Spawn => true,
            RuleActionKind::Event
            | RuleActionKind::Variable
            | RuleActionKind::ChangeScene
            | RuleActionKind::LoadScene
            | RuleActionKind::UnloadScene
            | RuleActionKind::NextScene
            | RuleActionKind::State => false,
        }
}

/// Gives the owner, and every UI or audio object a rule can name, a stable
/// ID. With `all_named`, every named object gets one, so object pickers
/// can point at it.
fn ensure_rule_object_ids(world: &mut World, owner: Entity, all_named: bool) {
    let missing: Vec<_> = world
        .iter_entities()
        .filter(|entity| {
            entity.get::<SceneObjectId>().is_none()
                && (entity.id() == owner
                    || (entity.get::<Name>().is_some()
                        && (all_named
                            || entity.get::<SceneUi>().is_some()
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

/// Whether any rule points at an object by ID, so object IDs must exist.
fn rules_name_objects(rules: &[RuleData]) -> bool {
    rules.iter().any(|rule| {
        matches!(
            rule.trigger,
            RuleTrigger::CollisionEnter { with: Some(_) }
                | RuleTrigger::CollisionExit { with: Some(_) }
                | RuleTrigger::TriggerEnter { with: Some(_) }
                | RuleTrigger::TriggerExit { with: Some(_) }
        ) || matches!(
            rule.action,
            RuleAction::SetVisible { .. }
                | RuleAction::SetEnabled { .. }
                | RuleAction::Destroy { .. }
                | RuleAction::SpawnPrefab {
                    at: SpawnAt::Object(_),
                    ..
                }
        )
    })
}

/// The "When" choices, in picker order.
const TRIGGER_CHOICES: [&str; 11] = [
    "This button is clicked",
    "Scene starts",
    "Key pressed",
    "Touches an object",
    "Stops touching",
    "Enters a trigger",
    "Leaves a trigger",
    "Timer",
    "Variable changes",
    "Game state changes",
    "Named event",
];

/// The picker label for `trigger`.
fn trigger_choice_label(trigger: &RuleTrigger) -> &'static str {
    match trigger {
        RuleTrigger::UiClicked => TRIGGER_CHOICES[0],
        RuleTrigger::SceneStart => TRIGGER_CHOICES[1],
        RuleTrigger::KeyPressed { .. } => TRIGGER_CHOICES[2],
        RuleTrigger::CollisionEnter { .. } => TRIGGER_CHOICES[3],
        RuleTrigger::CollisionExit { .. } => TRIGGER_CHOICES[4],
        RuleTrigger::TriggerEnter { .. } => TRIGGER_CHOICES[5],
        RuleTrigger::TriggerExit { .. } => TRIGGER_CHOICES[6],
        RuleTrigger::Timer { .. } => TRIGGER_CHOICES[7],
        RuleTrigger::VariableChanged { .. } => TRIGGER_CHOICES[8],
        RuleTrigger::StateChanged { .. } => TRIGGER_CHOICES[9],
        RuleTrigger::Named(_) => TRIGGER_CHOICES[10],
    }
}

/// A new trigger of the kind a picker label names, with starting fields.
fn trigger_for_choice(label: &str) -> Option<RuleTrigger> {
    Some(match label {
        "This button is clicked" => RuleTrigger::UiClicked,
        "Scene starts" => RuleTrigger::SceneStart,
        "Key pressed" => RuleTrigger::KeyPressed {
            key: "Space".into(),
        },
        "Touches an object" => RuleTrigger::CollisionEnter { with: None },
        "Stops touching" => RuleTrigger::CollisionExit { with: None },
        "Enters a trigger" => RuleTrigger::TriggerEnter { with: None },
        "Leaves a trigger" => RuleTrigger::TriggerExit { with: None },
        "Timer" => RuleTrigger::Timer {
            seconds: 1.0,
            repeat: false,
        },
        "Variable changes" => RuleTrigger::VariableChanged {
            name: "score".into(),
        },
        "Game state changes" => RuleTrigger::StateChanged {
            state: engine_scene::GameStateName::Paused,
        },
        "Named event" => RuleTrigger::Named("game_start".into()),
        _ => return None,
    })
}

/// Adds a starting condition. False at the limit.
fn add_condition(rule: &mut RuleData) -> bool {
    if rule.conditions.len() >= engine_scene::MAX_RULE_CONDITIONS {
        return false;
    }
    rule.conditions.push(RuleCondition {
        variable: "score".into(),
        compare: RuleCompare::GreaterOrEqual,
        value: 1.0,
    });
    true
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
        .is_some_and(|ui| ui.0.kind.raises_clicks());
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
    if kind.needs_object()
        && !world
            .iter_entities()
            .any(|entity| rule_target_matches(world, entity.id(), kind))
    {
        return false;
    }
    ensure_rule_object_ids(world, owner, kind.needs_object());
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
        RuleActionKind::ChangeScene => RuleAction::ChangeScene {
            path: "scenes/level2.ron".into(),
        },
        RuleActionKind::LoadScene => RuleAction::LoadScene {
            path: "scenes/level2.ron".into(),
        },
        RuleActionKind::UnloadScene => RuleAction::UnloadScene {
            path: "scenes/level2.ron".into(),
        },
        RuleActionKind::NextScene => RuleAction::NextScene,
        RuleActionKind::Visible => {
            let Some(target) = target else { return false };
            RuleAction::SetVisible {
                target,
                visible: false,
            }
        }
        RuleActionKind::Enabled => {
            let Some(target) = target else { return false };
            RuleAction::SetEnabled {
                target,
                enabled: false,
            }
        }
        RuleActionKind::Spawn => RuleAction::SpawnPrefab {
            path: "assets/prefabs/coin.prefab".into(),
            at: SpawnAt::Position([0.0; 3]),
        },
        RuleActionKind::Destroy => {
            let Some(target) = target else { return false };
            RuleAction::Destroy { target }
        }
        RuleActionKind::State => RuleAction::SetGameState {
            state: engine_scene::GameStateName::GameOver,
        },
    };
    rules.push(RuleData {
        trigger,
        action,
        conditions: Vec::new(),
        match_mode: engine_scene::RuleMatch::All,
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

/// Picks one of the project's scenes, or takes a typed path when the list isn't
/// known. A path that isn't a scene is shown as missing.
fn scene_picker(ui: &mut egui::Ui, salt: (Entity, usize), path: &mut String, scenes: &[String]) {
    if scenes.is_empty() {
        crate::widgets::text_field(ui, path, "scenes/level2.ron");
        return;
    }
    let shown = if scenes.contains(path) {
        scene_label(path)
    } else {
        format!("Missing: {}", scene_label(path))
    };
    choice(("visual_rule_scene", salt), ui)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for scene in scenes {
                ui.selectable_value(path, scene.clone(), scene_label(scene));
            }
        });
}

fn scene_label(path: &str) -> String {
    crate::state::scene_display_name(std::path::Path::new(path))
}

/// Picks one of `options`, or "Any object" for `None`.
fn optional_target_picker(
    ui: &mut egui::Ui,
    salt: (Entity, usize),
    target: &mut Option<String>,
    options: &[(String, String)],
) {
    let selected = match target {
        None => "Any object",
        Some(id) => options
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map_or("Missing target", |(_, name)| name.as_str()),
    };
    choice(("visual_rule_with", salt), ui)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(target, None, "Any object");
            for (id, name) in options {
                ui.selectable_value(target, Some(id.clone()), name);
            }
        });
}

/// A two-way choice such as Show / Hide; returns whether `value` changed.
fn on_off_choice(ui: &mut egui::Ui, salt: (Entity, usize), value: &mut bool, on: &str, off: &str) {
    choice(("visual_rule_switch", salt), ui)
        .selected_text(if *value { on } else { off })
        .show_ui(ui, |ui| {
            ui.selectable_value(value, true, on);
            ui.selectable_value(value, false, off);
        });
}

fn state_choice(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    state: &mut engine_scene::GameStateName,
) {
    choice(id, ui)
        .selected_text(state.label())
        .show_ui(ui, |ui| {
            for candidate in engine_scene::GameStateName::ALL {
                ui.selectable_value(state, candidate, candidate.label());
            }
        });
}

fn trigger_fields(
    ui: &mut egui::Ui,
    salt: (Entity, usize),
    trigger: &mut RuleTrigger,
    object_targets: &[(String, String)],
) {
    match trigger {
        RuleTrigger::Named(name) => {
            property_row(ui, "Event", |ui| {
                crate::widgets::text_field(ui, name, "Event name");
            });
        }
        RuleTrigger::KeyPressed { key } => {
            property_row(ui, "Key", |ui| {
                choice(("visual_rule_key", salt), ui)
                    .selected_text(key.as_str())
                    .show_ui(ui, |ui| {
                        for candidate in engine_scripting::ScriptKey::ALL {
                            let name = candidate.name();
                            ui.selectable_value(key, name.to_owned(), name);
                        }
                    });
            });
        }
        RuleTrigger::CollisionEnter { with }
        | RuleTrigger::CollisionExit { with }
        | RuleTrigger::TriggerEnter { with }
        | RuleTrigger::TriggerExit { with } => {
            property_row(ui, "With", |ui| {
                optional_target_picker(ui, salt, with, object_targets);
            });
        }
        RuleTrigger::Timer { seconds, repeat } => {
            property_row(ui, "Seconds", |ui| {
                number(
                    ui,
                    egui::DragValue::new(seconds)
                        .speed(0.05)
                        .range(0.05..=3600.0),
                );
            });
            property_row(ui, "Repeat", |ui| {
                crate::widgets::checkbox(ui, &crate::theme::NIGHT, repeat, "")
                    .on_hover_text("Start again after each time it fires");
            });
        }
        RuleTrigger::VariableChanged { name } => {
            property_row(ui, "Variable", |ui| {
                crate::widgets::text_field(ui, name, "Variable name");
            });
        }
        RuleTrigger::StateChanged { state } => {
            property_row(ui, "State", |ui| {
                state_choice(ui, ("visual_rule_state_trigger", salt), state);
            });
        }
        RuleTrigger::UiClicked | RuleTrigger::SceneStart => {}
    }
}

/// The named objects a rule's pickers can list: `(stable ID, name)`.
#[derive(Clone, Copy)]
struct RuleTargets<'a> {
    text: &'a [(String, String)],
    audio: &'a [(String, String)],
    objects: &'a [(String, String)],
    scenes: &'a [String],
}

/// The project's scene files, for the "Go to scene" picker. The shell keeps
/// this resource up to date; it is not saved with the scene.
#[derive(bevy_ecs::resource::Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectScenes(pub Vec<String>);

fn rule_fields(
    ui: &mut egui::Ui,
    entity: Entity,
    index: usize,
    rule: &mut RuleData,
    owner_is_button: bool,
    targets: &RuleTargets<'_>,
) {
    let RuleTargets {
        text: text_targets,
        audio: audio_targets,
        objects: object_targets,
        scenes,
    } = *targets;
    property_row(ui, "When", |ui| {
        choice(("visual_rule_trigger", entity, index), ui)
            .selected_text(trigger_choice_label(&rule.trigger))
            .show_ui(ui, |ui| {
                for label in TRIGGER_CHOICES {
                    if label == TRIGGER_CHOICES[0] && !owner_is_button {
                        continue;
                    }
                    let current = trigger_choice_label(&rule.trigger) == label;
                    if ui.selectable_label(current, label).clicked()
                        && !current
                        && let Some(trigger) = trigger_for_choice(label)
                    {
                        rule.trigger = trigger;
                    }
                }
            });
    });
    trigger_fields(ui, (entity, index), &mut rule.trigger, object_targets);
    let salt = (entity, index);
    match &mut rule.action {
        RuleAction::SetUiText { target, text } => {
            property_row(ui, "Set text on", |ui| {
                rule_target_picker(ui, salt, target, text_targets);
            });
            property_row(ui, "Text", |ui| {
                crate::widgets::text_field(ui, text, "");
            });
            note(ui, "Write {score} to show a variable's value.");
        }
        RuleAction::PlayAudio { target } => {
            property_row(ui, "Play sound on", |ui| {
                rule_target_picker(ui, salt, target, audio_targets);
            });
        }
        RuleAction::EmitEvent { name } => {
            property_row(ui, "Emit event", |ui| {
                crate::widgets::text_field(ui, name, "Event name");
            });
        }
        RuleAction::SetVariable { .. } | RuleAction::AddToVariable { .. } => {
            property_row(ui, "Do", |ui| {
                variable_action_fields(ui, salt, &mut rule.action);
            });
        }
        RuleAction::ChangeScene { path } => {
            property_row(ui, "Go to scene", |ui| {
                scene_picker(ui, salt, path, scenes);
            });
        }
        RuleAction::LoadScene { path } => {
            property_row(ui, "Load on top", |ui| {
                scene_picker(ui, salt, path, scenes);
            });
        }
        RuleAction::UnloadScene { path } => {
            property_row(ui, "Unload", |ui| {
                scene_picker(ui, salt, path, scenes);
            });
        }
        RuleAction::NextScene => {
            note(ui, "Goes to the scene after this one in the Scenes window.");
        }
        RuleAction::SetVisible { target, visible } => {
            property_row(ui, "Object", |ui| {
                rule_target_picker(ui, salt, target, object_targets);
            });
            property_row(ui, "Do", |ui| {
                on_off_choice(ui, salt, visible, "Show", "Hide");
            });
        }
        RuleAction::SetEnabled { target, enabled } => {
            property_row(ui, "Object", |ui| {
                rule_target_picker(ui, salt, target, object_targets);
            });
            property_row(ui, "Do", |ui| {
                on_off_choice(ui, salt, enabled, "Enable", "Disable");
            });
        }
        RuleAction::SpawnPrefab { path, at } => {
            property_row(ui, "Spawn", |ui| {
                crate::widgets::text_field(ui, path, "assets/prefabs/coin.prefab");
            });
            property_row(ui, "At", |ui| {
                let mut at_object = matches!(at, SpawnAt::Object(_));
                let was_object = at_object;
                on_off_choice(ui, salt, &mut at_object, "An object", "A position");
                if at_object != was_object {
                    *at = if at_object {
                        SpawnAt::Object(
                            object_targets
                                .first()
                                .map(|(id, _)| id.clone())
                                .unwrap_or_default(),
                        )
                    } else {
                        SpawnAt::Position([0.0; 3])
                    };
                }
            });
            match at {
                SpawnAt::Object(target) => {
                    property_row(ui, "Object", |ui| {
                        rule_target_picker(ui, salt, target, object_targets);
                    });
                }
                SpawnAt::Position(position) => {
                    property_row(ui, "Position", |ui| {
                        for (axis, component) in
                            ["X", "Y", "Z"].into_iter().zip(position.iter_mut())
                        {
                            number(
                                ui,
                                egui::DragValue::new(component)
                                    .speed(0.1)
                                    .prefix(format!("{axis} ")),
                            );
                        }
                    });
                }
            }
        }
        RuleAction::Destroy { target } => {
            property_row(ui, "Destroy", |ui| {
                rule_target_picker(ui, salt, target, object_targets);
            });
        }
        RuleAction::SetGameState { state } => {
            property_row(ui, "Set state", |ui| {
                state_choice(ui, ("visual_rule_state_action", salt), state);
            });
        }
    }
    rule_condition_fields(ui, salt, rule);
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

fn rule_condition_fields(ui: &mut egui::Ui, salt: (Entity, usize), rule: &mut RuleData) {
    let palette = crate::theme::NIGHT;
    property_row(ui, "Only if", |ui| {
        let full = rule.conditions.len() >= engine_scene::MAX_RULE_CONDITIONS;
        if ui
            .add_enabled(
                !full,
                egui::Button::new("Add condition").fill(palette.hover),
            )
            .on_hover_text("Run this rule only when a variable matches")
            .on_disabled_hover_text("A rule can have up to 8 conditions")
            .clicked()
        {
            add_condition(rule);
        }
    });
    let mut remove = None;
    for (position, condition) in rule.conditions.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let height = ui.spacing().interact_size.y;
            let field = crate::widgets::field(ui, &mut condition.variable, 12.0, height)
                .hint_text("Variable")
                .desired_width(80.0);
            ui.add(field);
            egui::ComboBox::from_id_salt(("visual_rule_compare", salt, position))
                .width(48.0)
                .icon(crate::widgets::combo_chevron)
                .selected_text(condition.compare.symbol())
                .show_ui(ui, |ui| {
                    for compare in RuleCompare::ALL {
                        ui.selectable_value(&mut condition.compare, compare, compare.symbol());
                    }
                });
            number(ui, egui::DragValue::new(&mut condition.value).speed(0.1));
            if ui
                .button(egui::RichText::new("Remove").color(palette.danger))
                .clicked()
            {
                remove = Some(position);
            }
        });
    }
    if let Some(position) = remove {
        rule.conditions.remove(position);
    }
    if rule.conditions.len() >= 2 {
        property_row(ui, "Match", |ui| {
            let mut any = rule.match_mode == engine_scene::RuleMatch::Any;
            on_off_choice(ui, salt, &mut any, "Any of these", "All of these");
            rule.match_mode = if any {
                engine_scene::RuleMatch::Any
            } else {
                engine_scene::RuleMatch::All
            };
        });
    }
}

/// Replaces `entity`'s collider shape with `shape_for(width, height)` of its
/// sprite. The body and every other setting stay as authored; an entity
/// without a collider gets a Fixed one. False when nothing changed.
fn fit_sprite_shape(
    world: &mut World,
    entity: Entity,
    shape_for: impl FnOnce(f32, f32) -> ColliderShape,
) -> bool {
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
    let shape = shape_for(width, height);
    let existing = world.get::<SceneCollider>(entity).map(|c| c.0.clone());
    let data = match existing {
        Some(mut data) => {
            if data.shape == shape {
                return false;
            }
            data.shape = shape;
            data
        }
        None => engine_scene::ColliderData::new(shape, BodyKind::Fixed),
    };
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return false;
    };
    entity_mut.insert(SceneCollider(data));
    true
}

fn fit_sprite_collider(world: &mut World, entity: Entity) -> bool {
    fit_sprite_shape(world, entity, |width, height| ColliderShape::Cuboid {
        half_extents: [width * 0.5, height * 0.5, 0.05],
    })
}

fn fit_sprite_circle(world: &mut World, entity: Entity) -> bool {
    fit_sprite_shape(world, entity, |width, height| ColliderShape::Ball {
        radius: width.max(height) * 0.5,
    })
}

/// The smallest near-plane distance the Inspector allows.
const MIN_NEAR: f32 = 0.001;

/// Sets the near plane (at least [`MIN_NEAR`]) and pushes far out when it
/// would no longer be beyond near.
fn set_camera_near(camera: &mut engine_renderer::Camera, near: f32) {
    camera.near = if near.is_finite() {
        near.max(MIN_NEAR)
    } else {
        MIN_NEAR
    };
    if camera.far <= camera.near {
        camera.far = camera.near + 1.0;
    }
}

/// Sets the far plane, keeping it beyond near.
fn set_camera_far(camera: &mut engine_renderer::Camera, far: f32) {
    camera.far = if far.is_finite() {
        far.max(camera.near + MIN_NEAR)
    } else {
        camera.near + 1.0
    };
}

/// Makes `entity` the active camera (removing the marker from every other
/// entity), or clears it from `entity` alone.
fn set_active_camera(world: &mut World, entity: Entity, active: bool) {
    if !active {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.remove::<ActiveCamera>();
        }
        return;
    }
    let others: Vec<Entity> = world
        .query_filtered::<Entity, With<ActiveCamera>>()
        .iter(world)
        .filter(|&other| other != entity)
        .collect();
    for other in others {
        if let Ok(mut other_mut) = world.get_entity_mut(other) {
            other_mut.remove::<ActiveCamera>();
        }
    }
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(ActiveCamera);
    }
}

/// Objects a camera can follow: named, with a transform, not the camera.
fn follow_targets(world: &World, camera: Entity) -> Vec<(String, String)> {
    world
        .iter_entities()
        .filter(|entity| entity.id() != camera && entity.contains::<Transform>())
        .filter_map(|entity| {
            let id = entity.get::<SceneObjectId>()?.0.clone();
            let name = entity.get::<Name>()?.0.clone();
            Some((id, name))
        })
        .collect()
}

/// One min/max degree pair that keeps `min < max`.
/// The choice of what input turns a follow camera.
fn follow_input_row(
    ui: &mut egui::Ui,
    entity: Entity,
    label: &str,
    input: &mut engine_ecs::components::OrbitInput,
) -> bool {
    use engine_ecs::components::OrbitInput;
    let name = |input: OrbitInput| match input {
        OrbitInput::None => "None",
        OrbitInput::Mouse => "Mouse",
        OrbitInput::RightStick => "Right stick",
        OrbitInput::Both => "Mouse and right stick",
    };
    let mut changed = false;
    property_row(ui, label, |ui| {
        choice(("camera_follow_input", entity), ui)
            .selected_text(name(*input))
            .show_ui(ui, |ui| {
                for option in [
                    OrbitInput::None,
                    OrbitInput::Mouse,
                    OrbitInput::RightStick,
                    OrbitInput::Both,
                ] {
                    changed |= ui.selectable_value(input, option, name(option)).changed();
                }
            });
    });
    changed
}

fn pitch_limit_rows(ui: &mut egui::Ui, min: &mut f32, max: &mut f32) -> bool {
    let mut changed = false;
    property_row(ui, "Pitch limits", |ui| {
        let low = number(
            ui,
            egui::DragValue::new(min)
                .prefix("Min ")
                .suffix("°")
                .speed(0.5)
                .range(-89.0..=88.0),
        )
        .changed();
        let high = number(
            ui,
            egui::DragValue::new(max)
                .prefix("Max ")
                .suffix("°")
                .speed(0.5)
                .range(-88.0..=89.0),
        )
        .changed();
        if low && *min >= *max {
            *max = *min + 1.0;
        }
        if high && *max <= *min {
            *min = *max - 1.0;
        }
        changed = low || high;
    });
    changed
}

/// The Camera Follow section: who to follow, and how.
fn camera_follow_rows(world: &mut World, entity: Entity, ui: &mut egui::Ui) -> bool {
    use engine_ecs::components::FollowMode;
    let Some((mut target, mut mode, mut switch)) = world
        .get::<CameraFollow>(entity)
        .map(|follow| (follow.target.clone(), follow.mode, follow.switch.clone()))
    else {
        return false;
    };
    let options = follow_targets(world, entity);
    let mut changed = false;

    property_row(ui, "Target", |ui| {
        let selected = if target.is_empty() {
            "Choose an object"
        } else {
            options
                .iter()
                .find(|(id, _)| *id == target)
                .map_or("Missing target", |(_, name)| name.as_str())
        };
        choice(("camera_follow_target", entity), ui)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for (id, name) in &options {
                    changed |= ui.selectable_value(&mut target, id.clone(), name).changed();
                }
            });
    });

    property_row(ui, "Mode", |ui| {
        let options = [
            (FollowMode::THIRD_PERSON, "Third person"),
            (FollowMode::FIRST_PERSON, "First person"),
            (FollowMode::SIDE_2D, "2D side"),
        ];
        let is_current =
            |option: &FollowMode| std::mem::discriminant(option) == std::mem::discriminant(&mode);
        let current = options
            .iter()
            .find(|(option, _)| is_current(option))
            .map_or("", |(_, name)| name);
        let mut picked = None;
        choice(("camera_follow_mode", entity), ui)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (option, name) in options {
                    let chosen = is_current(&option);
                    if ui.selectable_label(chosen, name).clicked() && !chosen {
                        picked = Some(option);
                    }
                }
            });
        if let Some(option) = picked {
            let same_view = |a: &FollowMode, b: &FollowMode| {
                std::mem::discriminant(a) == std::mem::discriminant(b)
            };
            match &mut switch {
                // The other view keeps its own settings.
                Some(switch) if same_view(&switch.other, &option) => {
                    std::mem::swap(&mut mode, &mut switch.other);
                }
                _ => mode = option,
            }
            if matches!(mode, FollowMode::Side2D { .. }) {
                switch = None;
            }
            changed = true;
        }
    });

    match &mut mode {
        FollowMode::ThirdPerson {
            distance,
            height,
            smoothing,
            orbit,
            pitch_min_degrees,
            pitch_max_degrees,
        } => {
            for (label, value, range) in [
                ("Distance", distance, 0.1..=1000.0),
                ("Height", height, -1000.0..=1000.0),
                ("Smoothing", smoothing, 0.0..=100.0),
            ] {
                property_row(ui, label, |ui| {
                    changed |=
                        number(ui, egui::DragValue::new(value).speed(0.1).range(range)).changed();
                });
            }
            changed |= follow_input_row(ui, entity, "Orbit with", orbit);
            changed |= pitch_limit_rows(ui, pitch_min_degrees, pitch_max_degrees);
        }
        FollowMode::FirstPerson {
            height,
            look,
            pitch_min_degrees,
            pitch_max_degrees,
        } => {
            property_row(ui, "Height", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(height)
                        .speed(0.1)
                        .range(-1000.0..=1000.0),
                )
                .changed();
            });
            changed |= follow_input_row(ui, entity, "Look with", look);
            changed |= pitch_limit_rows(ui, pitch_min_degrees, pitch_max_degrees);
        }
        FollowMode::Side2D {
            dead_zone,
            bounds,
            smoothing,
        } => {
            property_row(ui, "Dead zone", |ui| {
                for (axis, value) in ["X ", "Y "].into_iter().zip(dead_zone.iter_mut()) {
                    changed |= number(
                        ui,
                        egui::DragValue::new(value)
                            .prefix(axis)
                            .speed(0.1)
                            .range(0.0..=1000.0),
                    )
                    .changed();
                }
            });
            let mut limited = bounds.is_some();
            if crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut limited, "Limit to area")
                .on_hover_text("Keep the camera inside a rectangle")
                .changed()
            {
                *bounds = limited.then_some([-10.0, -10.0, 10.0, 10.0]);
                changed = true;
            }
            if let Some(area) = bounds {
                for (label, first) in [("Min", 0), ("Max", 2)] {
                    property_row(ui, label, |ui| {
                        for (axis, offset) in ["X ", "Y "].into_iter().zip(0..2) {
                            changed |= number(
                                ui,
                                egui::DragValue::new(&mut area[first + offset])
                                    .prefix(axis)
                                    .speed(0.1),
                            )
                            .changed();
                        }
                    });
                }
                // Keep min <= max whichever side was dragged.
                if area[0] > area[2] {
                    area.swap(0, 2);
                }
                if area[1] > area[3] {
                    area.swap(1, 3);
                }
            }
            property_row(ui, "Smoothing", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(smoothing)
                        .speed(0.1)
                        .range(0.0..=100.0),
                )
                .changed();
            });
        }
    }

    if !matches!(mode, FollowMode::Side2D { .. }) {
        let mut enabled = switch.is_some();
        if crate::widgets::checkbox(
            ui,
            &crate::theme::NIGHT,
            &mut enabled,
            "Switch view with a key",
        )
        .on_hover_text("Flip between first and third person while playing")
        .changed()
        {
            switch = enabled.then(|| engine_ecs::components::ViewSwitch {
                key: "V".to_owned(),
                other: if matches!(mode, FollowMode::FirstPerson { .. }) {
                    FollowMode::THIRD_PERSON
                } else {
                    FollowMode::FIRST_PERSON
                },
            });
            changed = true;
        }
        if let Some(switch) = &mut switch {
            property_row(ui, "Key", |ui| {
                choice(("camera_follow_switch_key", entity), ui)
                    .selected_text(switch.key.as_str())
                    .show_ui(ui, |ui| {
                        for candidate in engine_scripting::ScriptKey::ALL {
                            let name = candidate.name();
                            changed |= ui
                                .selectable_value(&mut switch.key, name.to_owned(), name)
                                .changed();
                        }
                    });
            });
        }
    }

    if changed && let Some(mut follow) = world.get_mut::<CameraFollow>(entity) {
        follow.target = target;
        follow.mode = mode;
        follow.switch = switch;
    }
    changed
}

/// Whether the Add-Component menu lists `component` for `entity`.
fn offered_in_add_menu(world: &World, entity: Entity, component: &ComponentUi) -> bool {
    if component.name == "Sprite" || (component.present)(world, entity) {
        return false;
    }
    // A follow moves a camera, so it is only offered on one.
    component.name != "Camera Follow" || world.get::<Camera>(entity).is_some()
}

/// The Camera section's property rows. `aspect_ratio` is left out (the
/// viewport owns it) and so are eye/target/up (the Transform aims the camera).
fn camera_rows(ui: &mut egui::Ui, entity: Entity, camera: &mut engine_renderer::Camera) -> bool {
    use engine_renderer::Projection;
    let mut changed = false;
    let perspective = matches!(camera.projection, Projection::Perspective { .. });
    property_row(ui, "Projection", |ui| {
        choice(("camera_projection", entity), ui)
            .selected_text(if perspective {
                "Perspective"
            } else {
                "Orthographic"
            })
            .show_ui(ui, |ui| {
                if ui.selectable_label(perspective, "Perspective").clicked() && !perspective {
                    camera.projection = Projection::Perspective {
                        fov_y_radians: 45.0_f32.to_radians(),
                    };
                    changed = true;
                }
                if ui.selectable_label(!perspective, "Orthographic").clicked() && perspective {
                    camera.projection = Projection::Orthographic { height: 10.0 };
                    changed = true;
                }
            });
    });
    match &mut camera.projection {
        Projection::Perspective { fov_y_radians } => {
            let mut degrees = fov_y_radians.to_degrees();
            property_row(ui, "Field of view", |ui| {
                if number(
                    ui,
                    egui::DragValue::new(&mut degrees)
                        .speed(0.5)
                        .range(10.0..=120.0)
                        .suffix("°"),
                )
                .changed()
                {
                    *fov_y_radians = degrees.clamp(10.0, 120.0).to_radians();
                    changed = true;
                }
            });
        }
        Projection::Orthographic { height } => {
            property_row(ui, "Size", |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(height).speed(0.1).range(0.1..=1000.0),
                )
                .changed();
            });
        }
    }
    let mut near = camera.near;
    property_row(ui, "Near", |ui| {
        if number(
            ui,
            egui::DragValue::new(&mut near)
                .speed(0.01)
                .range(MIN_NEAR..=100_000.0),
        )
        .changed()
        {
            set_camera_near(camera, near);
            changed = true;
        }
    });
    let mut far = camera.far;
    property_row(ui, "Far", |ui| {
        if number(
            ui,
            egui::DragValue::new(&mut far)
                .speed(1.0)
                .range(MIN_NEAR..=1_000_000.0),
        )
        .changed()
        {
            set_camera_far(camera, far);
            changed = true;
        }
    });
    changed
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
            // Eye at the origin looking down -Z: the object's Transform alone
            // decides the view, so nothing is lost when the scene is saved.
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(default_camera());
            }
        },
        remove: |world, entity| {
            // A follow only makes sense on a camera.
            strip::<CameraFollow>(world, entity);
            strip::<Camera>(world, entity);
        },
        inspect: |world, entity, ui, _assets| {
            let Some(mut value) = world.get::<Camera>(entity).map(|camera| camera.0) else {
                return false;
            };
            let mut changed = camera_rows(ui, entity, &mut value);
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(Camera::from(value));
            }
            let mut active = world.get::<ActiveCamera>(entity).is_some();
            if crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut active, "Active camera")
                .on_hover_text("The game looks through this camera")
                .changed()
            {
                set_active_camera(world, entity, active);
                changed = true;
            }
            changed
        },
        copy_to: copy_component::<Camera>,
    },
    ComponentUi {
        name: "Camera Follow",
        type_name: "CameraFollow",
        present: has::<CameraFollow>,
        insert_default: |world, entity| {
            if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(CameraFollow::new(
                    String::new(),
                    engine_ecs::components::FollowMode::THIRD_PERSON,
                ));
            }
        },
        remove: strip::<CameraFollow>,
        inspect: |world, entity, ui, _assets| camera_follow_rows(world, entity, ui),
        copy_to: copy_component::<CameraFollow>,
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
                entity_mut.insert(SceneCollider(engine_scene::ColliderData::new(
                    ColliderShape::Cuboid {
                        half_extents: [0.5, 0.5, 0.5],
                    },
                    BodyKind::Fixed,
                )));
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
            let body = value.0.body;
            let trigger = value.0.trigger;
            let dynamic = body == BodyKind::Dynamic;
            property_row(ui, "Trigger", |ui| {
                changed |=
                    crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut value.0.trigger, "")
                        .changed();
            });
            if dynamic && !trigger {
                property_row(ui, "Mass", |ui| {
                    let mut auto = value.0.mass.is_none();
                    if crate::widgets::checkbox(ui, &crate::theme::NIGHT, &mut auto, "Auto")
                        .changed()
                    {
                        value.0.mass = if auto { None } else { Some(1.0) };
                        changed = true;
                    }
                    if let Some(mass) = &mut value.0.mass {
                        changed |= number(
                            ui,
                            egui::DragValue::new(mass)
                                .speed(0.05)
                                .range(0.001..=100000.0),
                        )
                        .changed();
                    }
                });
            }
            if !trigger {
                let unit = |ui: &mut egui::Ui, label: &str, value: &mut f32, max: f32| {
                    property_row(ui, label, |ui| {
                        number(ui, egui::DragValue::new(value).speed(0.01).range(0.0..=max))
                            .changed()
                    })
                };
                changed |= unit(ui, "Friction", &mut value.0.friction, 10.0);
                changed |= unit(ui, "Bounciness", &mut value.0.bounciness, 1.0);
            }
            if dynamic && !trigger {
                property_row(ui, "Gravity", |ui| {
                    changed |= number(
                        ui,
                        egui::DragValue::new(&mut value.0.gravity_scale)
                            .speed(0.05)
                            .range(-10.0..=10.0),
                    )
                    .changed();
                });
            }
            if dynamic {
                property_row(ui, "Keep upright", |ui| {
                    changed |= crate::widgets::checkbox(
                        ui,
                        &crate::theme::NIGHT,
                        &mut value.0.lock_rotation,
                        "",
                    )
                    .changed();
                });
            }
            let (mut fit_box, mut fit_circle) = (false, false);
            if has_sprite {
                ui.horizontal_wrapped(|ui| {
                    fit_box = ui.button("Fit box to sprite").clicked();
                    fit_circle = ui.button("Fit circle to sprite").clicked();
                });
            }
            if changed && let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                entity_mut.insert(value);
            }
            if fit_box {
                changed |= fit_sprite_collider(world, entity);
            }
            if fit_circle {
                changed |= fit_sprite_circle(world, entity);
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
            if world.get::<SceneCollider>(entity).is_none() {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Add box collision").clicked() {
                        changed |= fit_sprite_collider(world, entity);
                    }
                    if ui.button("Add circle collision").clicked() {
                        changed |= fit_sprite_circle(world, entity);
                    }
                });
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
        inspect: |world, entity, ui, assets| {
            let Some(mut data) = world.get::<SceneUi>(entity).map(|value| value.0.clone()) else {
                return false;
            };
            let in_layout = in_ui_layout(world, entity);
            let object_id = world
                .get::<engine_scene::SceneObjectId>(entity)
                .map(|id| id.0.clone());
            let changed = game_ui_rows(ui, entity, &mut data, in_layout, object_id, assets);
            if data.text.len() > 1024 || data.placeholder.len() > 1024 {
                ui.colored_label(
                    crate::theme::NIGHT.danger,
                    "Text is too long for a scene UI element",
                );
                return false;
            }
            if !data.variable.is_empty() && !engine_scene::valid_variable_name(&data.variable) {
                ui.colored_label(
                    crate::theme::NIGHT.danger,
                    "A variable name is at most 64 characters",
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

/// Whether `entity`'s nearest UI ancestor is a Layout, which then places it.
fn in_ui_layout(world: &World, entity: Entity) -> bool {
    let mut current = world
        .get::<bevy_ecs::hierarchy::ChildOf>(entity)
        .map(bevy_ecs::hierarchy::ChildOf::parent);
    while let Some(ancestor) = current {
        if let Some(element) = world.get::<SceneUi>(ancestor) {
            return element.0.kind == UiKind::Layout;
        }
        current = world
            .get::<bevy_ecs::hierarchy::ChildOf>(ancestor)
            .map(bevy_ecs::hierarchy::ChildOf::parent);
    }
    false
}

/// The Game UI section's rows for one element. Returns whether anything changed.
fn game_ui_rows(
    ui: &mut egui::Ui,
    entity: Entity,
    data: &mut UiElementData,
    in_layout: bool,
    object_id: Option<String>,
    assets: &AssetIndex,
) -> bool {
    let mut changed = false;
    property_row(ui, "Widget", |ui| {
        choice(("game_ui_widget", entity), ui)
            .selected_text(crate::menu::ui_kind_name(data.kind))
            .show_ui(ui, |ui| {
                for kind in crate::menu::UI_KINDS {
                    if ui
                        .selectable_label(data.kind == kind, crate::menu::ui_kind_name(kind))
                        .clicked()
                        && data.kind != kind
                    {
                        // A new look and range, but it stays where it was
                        // and keeps its words.
                        let fresh = UiElementData::for_kind(kind);
                        *data = UiElementData {
                            text: std::mem::take(&mut data.text),
                            anchor: data.anchor,
                            offset: data.offset,
                            ..fresh
                        };
                        changed = true;
                    }
                }
            });
    });
    if data.kind.has_text() {
        let label = if data.kind == UiKind::TextInput {
            "Starting text"
        } else {
            "Text"
        };
        property_row(ui, label, |ui| {
            changed |= crate::widgets::text_field(ui, &mut data.text, "").changed();
        });
        if data.kind == UiKind::TextInput {
            property_row(ui, "Hint", |ui| {
                changed |= crate::widgets::text_field(ui, &mut data.placeholder, "").changed();
            });
        }
        property_row(ui, "Font size", |ui| {
            changed |= number(
                ui,
                egui::DragValue::new(&mut data.font_size)
                    .speed(0.5)
                    .range(4.0..=256.0),
            )
            .changed();
        });
    }
    if data.kind.has_text() {
        property_row(ui, "Bold", |ui| {
            changed |= ui.checkbox(&mut data.bold, "").changed();
        });
    }
    if data.kind == UiKind::Image {
        let name = data
            .image
            .as_ref()
            .and_then(|image| crate::assets::parse_asset_id(&image.id))
            .and_then(|id| assets.path_for(id))
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("No image — drop a PNG or JPEG");
        if let Some(path) = sprite_image_slot(ui, name)
            && let Some(id) = assets.id_for(&path)
        {
            data.image = Some(engine_scene::AssetRef { id: id.to_string() });
            changed = true;
        }
    }
    if in_layout {
        property_row(ui, "Position", |ui| {
            ui.label(egui::RichText::new("Set by the Layout").color(crate::theme::NIGHT.subtle));
        });
    } else {
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
        property_row(ui, "Offset", |ui| {
            for value in &mut data.offset {
                changed |= number(ui, egui::DragValue::new(value).speed(1.0)).changed();
            }
        });
    }
    property_row(ui, "Size", |ui| {
        for value in &mut data.size {
            changed |= number(
                ui,
                egui::DragValue::new(value).speed(1.0).range(1.0..=10000.0),
            )
            .changed();
        }
    });
    if !matches!(data.kind, UiKind::Label | UiKind::Slider) {
        property_row(ui, "Corner radius", |ui| {
            changed |= number(
                ui,
                egui::DragValue::new(&mut data.corner_radius)
                    .speed(0.5)
                    .range(0.0..=10000.0),
            )
            .changed();
        });
    }
    let main_color = match data.kind {
        UiKind::Label => "Text color",
        UiKind::Image => "Tint",
        _ => "Background",
    };
    property_row(ui, main_color, |ui| {
        changed |= ui
            .color_edit_button_rgba_unmultiplied(&mut data.color)
            .changed();
    });
    if matches!(
        data.kind,
        UiKind::Button | UiKind::Toggle | UiKind::TextInput
    ) {
        property_row(ui, "Text color", |ui| {
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut data.text_color)
                .changed();
        });
    }
    if data.kind.uses_variable() {
        property_row(ui, "Fill", |ui| {
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut data.fill_color)
                .changed();
        });
        property_row(ui, "Variable", |ui| {
            changed |= crate::widgets::text_field(ui, &mut data.variable, "health").changed();
        });
    }
    if matches!(data.kind, UiKind::ProgressBar | UiKind::Slider) {
        property_row(ui, "Range", |ui| {
            changed |= number(ui, egui::DragValue::new(&mut data.min).speed(0.1)).changed();
            changed |= number(ui, egui::DragValue::new(&mut data.max).speed(0.1)).changed();
        });
        // The scene format needs max above min.
        if data.max <= data.min {
            data.max = data.min + 1.0;
        }
    }
    if data.kind == UiKind::Slider {
        property_row(ui, "Step", |ui| {
            changed |= number(
                ui,
                egui::DragValue::new(&mut data.step)
                    .speed(0.01)
                    .range(0.0..=f32::MAX),
            )
            .changed();
        });
    }
    if data.kind == UiKind::Layout {
        property_row(ui, "Direction", |ui| {
            choice(("game_ui_direction", entity), ui)
                .selected_text(format!("{:?}", data.direction))
                .show_ui(ui, |ui| {
                    for direction in [
                        engine_scene::UiLayoutDirection::Vertical,
                        engine_scene::UiLayoutDirection::Horizontal,
                    ] {
                        changed |= ui
                            .selectable_value(
                                &mut data.direction,
                                direction,
                                format!("{direction:?}"),
                            )
                            .changed();
                    }
                });
        });
        for (label, value) in [("Gap", &mut data.gap), ("Padding", &mut data.padding)] {
            property_row(ui, label, |ui| {
                changed |= number(
                    ui,
                    egui::DragValue::new(value).speed(0.5).range(0.0..=10000.0),
                )
                .changed();
            });
        }
    }
    if (data.kind.raises_clicks())
        && let Some(id) = object_id
    {
        let label = if data.kind == UiKind::Button {
            "Click ID"
        } else {
            "Field ID"
        };
        property_row(ui, label, |ui| {
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(id.clone());
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(id.as_str()).color(crate::theme::NIGHT.subtle),
                )
                .truncate(),
            );
        });
    }
    changed
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
    // A locked object is shown but can't be changed; only its Locked tick works.
    let locked = has::<Lock>(world, entity);
    if locked {
        note(ui, "Locked. Untick Locked to change this object.");
    }
    ui.add_enabled_ui(!locked, |ui| {
        show_body(ui, world, entity, others, batch_edit, assets, open_script);
    });
}

/// Everything under the identity header.
fn show_body(
    ui: &mut egui::Ui,
    world: &mut World,
    entity: Entity,
    others: &[Entity],
    batch_edit: &mut bool,
    assets: &AssetIndex,
    open_script: &mut Option<String>,
) {
    let palette = crate::theme::NIGHT;
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
                if !has::<Lock>(world, other) {
                    (component.copy_to)(world, entity, other);
                }
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
        let all_named = rules_name_objects(&rules);
        ensure_rule_object_ids(world, entity, all_named);
    }
    let before = rules.clone();
    let owner_is_button = world
        .get::<SceneUi>(entity)
        .is_some_and(|value| value.0.kind.raises_clicks());
    let text_targets = rule_targets(world, RuleActionKind::Text);
    let audio_targets = rule_targets(world, RuleActionKind::Audio);
    let object_targets = rule_targets(world, RuleActionKind::Destroy);
    let scenes = world
        .get_resource::<ProjectScenes>()
        .map_or_else(Vec::new, |list| list.0.clone());
    let has_text_target = world
        .iter_entities()
        .any(|candidate| rule_target_matches(world, candidate.id(), RuleActionKind::Text));
    let has_audio_target = world
        .iter_entities()
        .any(|candidate| rule_target_matches(world, candidate.id(), RuleActionKind::Audio));
    let has_object_target = world
        .iter_entities()
        .any(|candidate| rule_target_matches(world, candidate.id(), RuleActionKind::Destroy));
    let can_add_rule = rules.len() < 16 && visual_rule_count(world) < 1024;
    let mut add_rule = None;
    let mut remove_rule = None;
    section(ui, "Visual Rules", None::<fn(&mut egui::Ui)>, |ui| {
        note(
            ui,
            "React to a button, a key, a timer, a game event, and more.",
        );
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
                        ("Change scene", RuleActionKind::ChangeScene, true),
                        ("Load a scene on top", RuleActionKind::LoadScene, true),
                        ("Unload a scene", RuleActionKind::UnloadScene, true),
                        ("Go to the next scene", RuleActionKind::NextScene, true),
                        (
                            "Show or hide object",
                            RuleActionKind::Visible,
                            has_object_target,
                        ),
                        (
                            "Enable or disable object",
                            RuleActionKind::Enabled,
                            has_object_target,
                        ),
                        ("Spawn prefab", RuleActionKind::Spawn, true),
                        ("Destroy object", RuleActionKind::Destroy, has_object_target),
                        ("Set game state", RuleActionKind::State, true),
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
                        &RuleTargets {
                            text: &text_targets,
                            audio: &audio_targets,
                            objects: &object_targets,
                            scenes: &scenes,
                        },
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
            object_targets: &object_targets,
            scenes: &scenes,
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
/// its id, and the Disabled / Locked markers.
fn identity_header(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    let palette = crate::theme::NIGHT;
    let locked = has::<Lock>(world, entity);
    let mut name = name_of(world, entity).unwrap_or_default();
    let field = crate::widgets::field(ui, &mut name, 14.0, 30.0)
        .hint_text("Name")
        .desired_width(ui.available_width());
    let field = ui.add_enabled(!locked, field);
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
        let mut disabled = has::<Disabled>(world, entity);
        let disable = ui
            .add_enabled_ui(!locked, |ui| {
                crate::widgets::checkbox(ui, &palette, &mut disabled, "Disabled")
            })
            .inner;
        if disable.changed() {
            toggle_marker::<Disabled>(world, entity, disabled);
        }
        let mut lock_tick = locked;
        if crate::widgets::checkbox(ui, &palette, &mut lock_tick, "Locked").changed() {
            toggle_marker::<Lock>(world, entity, lock_tick);
        }
        // Only a top-level object can be kept; children go with their parent.
        let top_level = world.get::<engine_ecs::prelude::ChildOf>(entity).is_none();
        let mut kept = top_level && has::<engine_scene::KeepBetweenScenes>(world, entity);
        let keep = ui
            .add_enabled_ui(top_level && !locked, |ui| {
                crate::widgets::checkbox(ui, &palette, &mut kept, "Keep between scenes")
            })
            .inner
            .on_hover_text("This object stays when the game changes scene.")
            .on_disabled_hover_text("A child object goes with its parent.");
        if keep.changed() {
            toggle_marker::<engine_scene::KeepBetweenScenes>(world, entity, kept);
        }
    });
    ui.add_enabled_ui(!locked, |ui| tags_row(ui, world, entity));
}

/// The object's tags as removable chips, with a field to add one.
fn tags_row(ui: &mut egui::Ui, world: &mut World, entity: Entity) {
    let palette = crate::theme::NIGHT;
    let mut tags = world
        .get::<engine_scene::SceneTags>(entity)
        .map_or_else(Vec::new, |tags| tags.0.clone());
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        let mut removed = None;
        for (index, tag) in tags.iter().enumerate() {
            let chip = egui::Button::new(
                egui::RichText::new(format!("{tag}  ×"))
                    .size(11.5)
                    .color(palette.text),
            )
            .fill(palette.accent_fill)
            .corner_radius(9);
            if ui.add(chip).on_hover_text("Remove this tag").clicked() {
                removed = Some(index);
            }
        }
        if let Some(index) = removed {
            tags.remove(index);
            changed = true;
        }
        let draft_id = ui.id().with(("new-tag", entity));
        let mut draft: String = ui.data_mut(|data| data.get_temp(draft_id).unwrap_or_default());
        let field = ui.add(
            crate::widgets::field(ui, &mut draft, 11.5, 22.0)
                .hint_text("Add tag")
                .desired_width(84.0),
        );
        let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if submitted && add_tag(&mut tags, &draft) {
            changed = true;
            draft.clear();
        }
        ui.data_mut(|data| data.insert_temp(draft_id, draft));
    });
    if changed {
        if tags.is_empty() {
            toggle_marker::<engine_scene::SceneTags>(world, entity, false);
        } else if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert(engine_scene::SceneTags(tags));
        }
    }
}

/// Adds `text` to `tags` unless it is blank, too long, a repeat, or there are
/// already as many as an object may have. Returns whether it was added.
fn add_tag(tags: &mut Vec<String>, text: &str) -> bool {
    let tag = text.trim();
    let usable = !tag.is_empty()
        && tag.len() <= engine_scene::MAX_TAG_BYTES
        && tags.len() < engine_scene::MAX_TAGS
        && !tags.iter().any(|existing| existing == tag);
    if usable {
        tags.push(tag.to_owned());
    }
    usable
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
            if !offered_in_add_menu(world, entity, component) {
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
                conditions: Vec::new(),
                match_mode: engine_scene::RuleMatch::All,
                graph_position: None,
            },
            RuleData {
                trigger: RuleTrigger::Named("score_changed".into()),
                action: RuleAction::SetUiText {
                    target,
                    text: "Score".into(),
                },
                conditions: Vec::new(),
                match_mode: engine_scene::RuleMatch::All,
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
                       graph_position: None,
        ..
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
            conditions: Vec::new(),
            match_mode: engine_scene::RuleMatch::All,
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
                       graph_position: None,
        ..
                   } if trigger == "game_start" && name == "next_event"
               ));
    }

    #[test]
    fn new_trigger_choice_sets_default_fields() {
        assert_eq!(
            trigger_for_choice("Timer"),
            Some(RuleTrigger::Timer {
                seconds: 1.0,
                repeat: false
            })
        );
        assert_eq!(
            trigger_for_choice("Key pressed"),
            Some(RuleTrigger::KeyPressed {
                key: "Space".into()
            })
        );
        assert_eq!(
            trigger_for_choice("Touches an object"),
            Some(RuleTrigger::CollisionEnter { with: None })
        );
        assert_eq!(
            trigger_for_choice("Game state changes"),
            Some(RuleTrigger::StateChanged {
                state: engine_scene::GameStateName::Paused
            })
        );
        assert_eq!(trigger_for_choice("Nonsense"), None);
        for label in TRIGGER_CHOICES {
            let trigger = trigger_for_choice(label).expect("every choice has a default");
            assert_eq!(trigger_choice_label(&trigger), label);
        }
    }

    #[test]
    fn add_rule_offers_new_action_kinds() {
        for kind in [
            RuleActionKind::ChangeScene,
            RuleActionKind::Spawn,
            RuleActionKind::State,
        ] {
            let mut world = World::new();
            let owner = world.spawn(Name::new("Rules")).id();
            assert!(add_visual_rule(&mut world, owner, kind, None));
            let rules = &world.get::<SceneRules>(owner).expect("rules").0;
            assert!(
                matches!(
                    (&rules[0].action, kind),
                    (RuleAction::ChangeScene { path }, RuleActionKind::ChangeScene)
                        if path == "scenes/level2.ron"
                ) || matches!(
                    (&rules[0].action, kind),
                    (RuleAction::SpawnPrefab { .. }, RuleActionKind::Spawn)
                ) || matches!(
                    (&rules[0].action, kind),
                    (RuleAction::SetGameState { .. }, RuleActionKind::State)
                )
            );
        }
        let mut lonely = World::new();
        let owner = lonely.spawn_empty().id();
        for kind in [
            RuleActionKind::Visible,
            RuleActionKind::Enabled,
            RuleActionKind::Destroy,
        ] {
            assert!(!add_visual_rule(&mut lonely, owner, kind, None));
        }
        assert!(lonely.get::<SceneRules>(owner).is_none());
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let door = world.spawn(Name::new("Door")).id();
        for kind in [
            RuleActionKind::Visible,
            RuleActionKind::Enabled,
            RuleActionKind::Destroy,
        ] {
            assert!(add_visual_rule(&mut world, owner, kind, None));
        }
        let door_id = world.get::<SceneObjectId>(door).expect("id").0.clone();
        let rules = &world.get::<SceneRules>(owner).expect("rules").0;
        assert!(matches!(&rules[0].action,
            RuleAction::SetVisible { target, visible: false } if *target == door_id));
        assert!(matches!(&rules[1].action,
            RuleAction::SetEnabled { target, enabled: false } if *target == door_id));
        assert!(matches!(&rules[2].action,
            RuleAction::Destroy { target } if *target == door_id));
    }

    #[test]
    fn a_disabled_object_can_be_picked_for_enable() {
        let mut world = World::new();
        let owner = world.spawn_empty().id();
        let door = world.spawn((Name::new("Door"), Disabled)).id();
        assert!(add_visual_rule(
            &mut world,
            owner,
            RuleActionKind::Enabled,
            None
        ));
        let door_id = world.get::<SceneObjectId>(door).expect("id").0.clone();
        assert!(matches!(
            &world.get::<SceneRules>(owner).expect("rules").0[0].action,
            RuleAction::SetEnabled { target, .. } if *target == door_id
        ));
        assert!(rule_targets(&world, RuleActionKind::Destroy).contains(&(door_id, "Door".into())));
    }

    #[test]
    fn condition_list_adds_up_to_eight() {
        let mut rule = RuleData::new(
            RuleTrigger::SceneStart,
            RuleAction::EmitEvent { name: "x".into() },
        );
        for _ in 0..engine_scene::MAX_RULE_CONDITIONS {
            assert!(add_condition(&mut rule));
        }
        assert!(!add_condition(&mut rule));
        assert_eq!(rule.conditions.len(), 8);
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

    fn spawn_sprite_collider(size: [f32; 2], data: engine_scene::ColliderData) -> (World, Entity) {
        let mut world = World::new();
        let entity = world
            .spawn((
                SpriteSource {
                    atlas: uuid::Uuid::nil().to_string(),
                    region: "full".into(),
                    size,
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                SceneCollider(data),
            ))
            .id();
        (world, entity)
    }

    #[test]
    fn fitting_a_sprite_keeps_body_settings() {
        let mut data =
            engine_scene::ColliderData::new(ColliderShape::Ball { radius: 1.0 }, BodyKind::Dynamic);
        data.friction = 0.9;
        data.trigger = true;
        data.mass = Some(3.0);
        data.bounciness = 0.4;
        data.gravity_scale = 0.5;
        data.lock_rotation = true;
        let (mut world, entity) = spawn_sprite_collider([4.0, 2.0], data.clone());
        assert!(fit_sprite_collider(&mut world, entity));
        let fitted = &world.get::<SceneCollider>(entity).unwrap().0;
        assert_eq!(
            fitted.shape,
            ColliderShape::Cuboid {
                half_extents: [2.0, 1.0, 0.05]
            }
        );
        let expected = engine_scene::ColliderData {
            shape: fitted.shape.clone(),
            ..data
        };
        assert_eq!(fitted, &expected);
    }

    #[test]
    fn fit_sprite_circle_uses_the_larger_side() {
        let mut data = engine_scene::ColliderData::new(
            ColliderShape::Cuboid {
                half_extents: [1.0; 3],
            },
            BodyKind::Dynamic,
        );
        data.friction = 0.9;
        data.trigger = true;
        data.mass = Some(3.0);
        let (mut world, entity) = spawn_sprite_collider([2.0, 1.0], data.clone());
        assert!(fit_sprite_circle(&mut world, entity));
        let fitted = &world.get::<SceneCollider>(entity).unwrap().0;
        assert_eq!(fitted.shape, ColliderShape::Ball { radius: 1.0 });
        assert_eq!(fitted.body, BodyKind::Dynamic);
        assert_eq!(fitted.friction, 0.9);
        assert!(fitted.trigger);
        assert_eq!(fitted.mass, Some(3.0));
        assert!(!fit_sprite_circle(&mut world, entity));
        world.get_mut::<SpriteSource>(entity).unwrap().size = [f32::NAN, 1.0];
        assert!(!fit_sprite_circle(&mut world, entity));
    }

    #[test]
    fn fit_sprite_circle_adds_a_fixed_circle_when_there_is_no_collider() {
        let mut world = World::new();
        let entity = world
            .spawn(SpriteSource {
                atlas: uuid::Uuid::nil().to_string(),
                region: "full".into(),
                size: [1.0, 3.0],
                color: [1.0; 4],
                z_order: 0.0,
            })
            .id();
        assert!(fit_sprite_circle(&mut world, entity));
        let data = &world.get::<SceneCollider>(entity).unwrap().0;
        assert_eq!(data.shape, ColliderShape::Ball { radius: 1.5 });
        assert_eq!(data.body, BodyKind::Fixed);
    }

    /// The labels the Collider section draws for `data`.
    fn collider_labels(data: engine_scene::ColliderData) -> Vec<String> {
        let mut world = World::new();
        let entity = world.spawn(SceneCollider(data)).id();
        let section = REGISTRY
            .iter()
            .find(|component| component.name == "Collider")
            .expect("Collider registry entry");
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let assets = AssetIndex::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            (section.inspect)(&mut world, entity, ui, &assets);
        });
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    /// The labels the Camera section draws for `camera`.
    fn camera_labels(camera: engine_renderer::Camera) -> Vec<String> {
        let mut world = World::new();
        let entity = world.spawn(Camera::from(camera)).id();
        let section = REGISTRY
            .iter()
            .find(|component| component.name == "Camera")
            .expect("Camera registry entry");
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let assets = AssetIndex::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            (section.inspect)(&mut world, entity, ui, &assets);
        });
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn camera_rows_follow_projection() {
        let has = |labels: &[String], name: &str| labels.iter().any(|label| label == name);
        let perspective = camera_labels(engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0));
        assert!(has(&perspective, "Field of view") && !has(&perspective, "Size"));
        let ortho = camera_labels(engine_renderer::Camera::new_orthographic(
            Vec3::ZERO,
            Vec3::NEG_Z,
            1.0,
            10.0,
        ));
        assert!(has(&ortho, "Size") && !has(&ortho, "Field of view"));
        for labels in [&perspective, &ortho] {
            assert!(has(labels, "Projection"));
            assert!(has(labels, "Near") && has(labels, "Far") && has(labels, "Active camera"));
            for gone in ["Eye", "Target", "Up"] {
                assert!(!has(labels, gone), "{gone} row must not be shown");
            }
        }
    }

    #[test]
    fn ticking_active_camera_clears_others() {
        use engine_ecs::components::ActiveCamera;
        let mut world = World::new();
        let cam = || Camera::from(engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0));
        let first = world.spawn((cam(), ActiveCamera)).id();
        let second = world.spawn(cam()).id();
        set_active_camera(&mut world, second, true);
        assert!(world.get::<ActiveCamera>(first).is_none());
        assert!(world.get::<ActiveCamera>(second).is_some());
        set_active_camera(&mut world, second, false);
        assert!(world.get::<ActiveCamera>(second).is_none());
    }

    #[test]
    fn unticking_active_camera_only_clears_this_one() {
        use engine_ecs::components::ActiveCamera;
        let mut world = World::new();
        let cam = || Camera::from(engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0));
        let first = world.spawn((cam(), ActiveCamera)).id();
        let second = world.spawn((cam(), ActiveCamera)).id();
        set_active_camera(&mut world, first, false);
        assert!(world.get::<ActiveCamera>(first).is_none());
        assert!(
            world.get::<ActiveCamera>(second).is_some(),
            "the other camera keeps its marker"
        );
    }

    #[test]
    fn camera_follow_section_only_for_cameras() {
        let mut world = World::new();
        let plain = world.spawn(Transform::from(MathTransform::IDENTITY)).id();
        let camera = world
            .spawn(Camera::from(engine_renderer::Camera::new(
                Vec3::ZERO,
                Vec3::NEG_Z,
                1.0,
            )))
            .id();
        let follow = REGISTRY
            .iter()
            .find(|component| component.name == "Camera Follow")
            .expect("Camera Follow registry entry");
        assert!(!offered_in_add_menu(&world, plain, follow));
        assert!(offered_in_add_menu(&world, camera, follow));
        (follow.insert_default)(&mut world, camera);
        assert!(
            !offered_in_add_menu(&world, camera, follow),
            "already added"
        );
        // Removing the camera takes its follow with it.
        let camera_ui = REGISTRY
            .iter()
            .find(|component| component.name == "Camera")
            .expect("Camera registry entry");
        (camera_ui.remove)(&mut world, camera);
        assert!(!(follow.present)(&world, camera));
    }

    /// The labels the Camera Follow section draws for `follow`.
    fn follow_labels(follow: engine_ecs::components::CameraFollow) -> Vec<String> {
        let mut world = World::new();
        let entity = world.spawn(follow).id();
        let section = REGISTRY
            .iter()
            .find(|component| component.name == "Camera Follow")
            .expect("Camera Follow registry entry");
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let assets = AssetIndex::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            (section.inspect)(&mut world, entity, ui, &assets);
        });
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn camera_follow_rows_follow_the_mode() {
        use engine_ecs::components::{CameraFollow, FollowMode};
        let has = |labels: &[String], name: &str| labels.iter().any(|label| label == name);
        let third = follow_labels(CameraFollow::new("", FollowMode::THIRD_PERSON));
        for row in [
            "Target",
            "Mode",
            "Distance",
            "Height",
            "Smoothing",
            "Orbit with",
        ] {
            assert!(has(&third, row), "missing {row}");
        }
        assert!(has(&third, "Pitch limits") && !has(&third, "Dead zone"));
        let side = follow_labels(CameraFollow::new("", FollowMode::SIDE_2D));
        for row in ["Target", "Mode", "Dead zone", "Smoothing", "Limit to area"] {
            assert!(has(&side, row), "missing {row}");
        }
        assert!(!has(&side, "Distance") && !has(&side, "Orbit with"));
    }

    #[test]
    fn far_stays_beyond_near() {
        let mut camera = engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0);
        camera.far = 10.0;
        set_camera_near(&mut camera, 50.0);
        assert!(camera.far > camera.near && camera.near > 0.0);
        set_camera_far(&mut camera, 1.0);
        assert!(camera.far > camera.near);
        set_camera_near(&mut camera, -3.0);
        assert!(camera.near > 0.0);
    }

    #[test]
    fn added_camera_view_survives_save_and_reload() {
        let mut world = World::new();
        let eye = Vec3::new(0.0, 2.0, 6.0);
        let mut transform = MathTransform::from_translation(eye);
        transform.rotation =
            engine_scene::look_rotation(Vec3::ZERO - eye, Vec3::Y).expect("look rotation");
        let entity = world
            .spawn((Name::new("Cam"), Transform::from(transform)))
            .id();
        let section = REGISTRY
            .iter()
            .find(|component| component.name == "Camera")
            .expect("Camera registry entry");
        (section.insert_default)(&mut world, entity);

        let matrix = transform.to_matrix();
        let (want_eye, want_target, _) = engine_scene::camera_world_pose(matrix);
        let scene = engine_scene::Scene::from_world(&mut world);
        let saved = scene.entities[0].camera.expect("saved camera");
        assert!(Vec3::from_array(saved.eye).distance(want_eye) < 1e-4);
        assert!(Vec3::from_array(saved.target).distance(want_target) < 1e-4);
        let forward = Vec3::from_array(saved.target) - Vec3::from_array(saved.eye);
        assert!(
            forward.distance((Vec3::ZERO - eye).normalize()) < 1e-3,
            "looks at the origin"
        );

        let mut reloaded = World::new();
        scene.instantiate(&mut reloaded);
        let mut query = reloaded.query::<(&Transform, &Camera)>();
        let (t, _) = query.single(&reloaded).expect("one camera");
        let (eye2, target2, _) = engine_scene::camera_world_pose(t.0.to_matrix());
        assert!(eye2.distance(want_eye) < 1e-4 && target2.distance(want_target) < 1e-4);
    }

    fn collider(body: BodyKind, trigger: bool) -> engine_scene::ColliderData {
        let mut data = engine_scene::ColliderData::new(ColliderShape::Ball { radius: 0.5 }, body);
        data.trigger = trigger;
        data
    }

    #[test]
    fn collider_rows_follow_body_kind() {
        let has = |labels: &[String], name: &str| labels.iter().any(|label| label == name);

        let fixed = collider_labels(collider(BodyKind::Fixed, false));
        assert!(has(&fixed, "Trigger"));
        assert!(has(&fixed, "Friction") && has(&fixed, "Bounciness"));
        assert!(!has(&fixed, "Mass") && !has(&fixed, "Gravity"));
        assert!(!has(&fixed, "Keep upright"));

        let dynamic = collider_labels(collider(BodyKind::Dynamic, false));
        assert!(has(&dynamic, "Mass") && has(&dynamic, "Gravity"));
        assert!(has(&dynamic, "Friction") && has(&dynamic, "Keep upright"));

        let kinematic = collider_labels(collider(BodyKind::Kinematic, false));
        // Rapier ignores locked rotation on script-moved Kinematic bodies.
        assert!(!has(&kinematic, "Keep upright"));
        assert!(!has(&kinematic, "Mass") && !has(&kinematic, "Gravity"));

        let trigger = collider_labels(collider(BodyKind::Dynamic, true));
        assert!(has(&trigger, "Trigger") && !has(&trigger, "Gravity"));
        assert!(!has(&trigger, "Mass"));
        assert!(!has(&trigger, "Friction") && !has(&trigger, "Bounciness"));
    }

    #[test]
    fn mass_row_offers_auto_and_a_value_when_not_auto() {
        let auto = collider_labels(collider(BodyKind::Dynamic, false));
        assert!(auto.iter().any(|label| label == "Auto"));
        let mut data = collider(BodyKind::Dynamic, false);
        data.mass = Some(2.5);
        let manual = collider_labels(data);
        assert!(manual.iter().any(|label| label == "Auto"));
        assert!(manual.iter().any(|label| label.contains("2.5")));
        assert!(!auto.iter().any(|label| label.contains("2.5")));
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
                SceneCollider(engine_scene::ColliderData::new(
                    ColliderShape::Ball { radius: 1.0 },
                    BodyKind::Dynamic,
                )),
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

    /// Drags across the whole panel, row by row, and returns the object's position.
    fn drag_over_inspector(world: &mut World, entity: Entity) -> Vec3 {
        let ctx = egui::Context::default();
        let (mut batch, mut script) = (false, None);
        let assets = AssetIndex::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 400.0));
        let mut frame = |world: &mut World, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    show(
                        ui,
                        world,
                        Some(entity),
                        &[],
                        &mut batch,
                        &assets,
                        &mut script,
                    )
                },
            );
            output.textures_delta.clear();
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(world, Vec::new());
        for y in (0..400).step_by(10) {
            for x in (40..400).step_by(60) {
                let start = egui::pos2(x as f32, y as f32);
                let end = start + egui::vec2(40.0, 0.0);
                frame(world, vec![egui::Event::PointerMoved(start)]);
                frame(world, vec![button(start, true)]);
                frame(world, vec![egui::Event::PointerMoved(end)]);
                frame(world, vec![button(end, false)]);
            }
        }
        world.get::<Transform>(entity).unwrap().0.translation
    }

    #[test]
    fn dragging_changes_an_object_only_when_it_is_not_locked() {
        let spawn = |world: &mut World| {
            world
                .spawn((
                    Name::new("Rock"),
                    Transform::from(MathTransform::from_translation(Vec3::ONE)),
                ))
                .id()
        };
        let mut world = World::new();
        let open = spawn(&mut world);
        assert_ne!(drag_over_inspector(&mut world, open), Vec3::ONE);

        let mut world = World::new();
        let locked = spawn(&mut world);
        world.entity_mut(locked).insert(Lock);
        assert_eq!(drag_over_inspector(&mut world, locked), Vec3::ONE);
    }

    #[test]
    fn toggle_marker_adds_and_removes() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        assert!(!has::<Disabled>(&world, entity));

        toggle_marker::<Disabled>(&mut world, entity, true);
        assert!(has::<Disabled>(&world, entity));

        toggle_marker::<Disabled>(&mut world, entity, false);
        assert!(!has::<Disabled>(&world, entity));
    }

    #[test]
    fn toggle_marker_on_a_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        world.despawn(entity);
        toggle_marker::<Disabled>(&mut world, entity, true); // must not panic
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
