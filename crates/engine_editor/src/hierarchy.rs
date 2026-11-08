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

use std::any::TypeId;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use engine_ecs::components::{
    AssetSource, Camera, Disabled, GlobalTransform, Hidden, Lock, MeshRenderer, MeshSource, Name,
    Sprite, SpriteSource, Static, Transform as TransformComponent, WaterVolume,
};
use engine_ecs::prelude::{ChildOf, Children, Entity, With, Without, World};
use engine_scene::SiblingOrder;
use engine_utils::Transform as MathTransform;
use glam::Vec3;

use crate::object_clipboard::{self, ObjectClipboard};

/// Deepest `ChildOf` chain walked before bailing — a malformed cycle
/// would otherwise stack-overflow `engine_ecs::propagate`, so every
/// traversal here is bounded.
const MAX_HIERARCHY_DEPTH: u32 = 256;

/// Cross-frame state for the hierarchy panel: the name filter and any
/// in-progress inline rename. Owned by [`crate::EditorState`].
#[derive(Debug, Default)]
pub struct HierarchyState {
    /// Case-insensitive substring the tree is filtered by; empty shows
    /// everything.
    pub search: String,
    /// The row whose name is currently being edited inline, if any.
    renaming: Option<Renaming>,
    /// Set when the user picks "Make Prefab" (toolbar or row menu). The
    /// shell drains it into
    /// [`crate::EditorState::create_prefab_request`] for the host to
    /// write the `.prefab` file.
    pub(crate) prefab_request: Option<Entity>,
    /// Set by double-clicking a row; the shell frames the selection in the
    /// Scene View.
    pub(crate) frame_request: bool,
    /// The primary selection when the panel last drew, to notice selections
    /// made elsewhere (such as clicking in the Scene View).
    last_selected: Option<Entity>,
    /// A newly selected entity to scroll into view this frame.
    reveal: Option<Entity>,
    /// Its ancestors, whose rows are expanded so it is visible.
    reveal_ancestors: Vec<Entity>,
    /// The visible rows in draw order, rebuilt each [`show`] — the basis
    /// for shift-click range selection.
    pub(crate) flat_order: Vec<Entity>,
    /// Rows the user unfolded, by stable object id ([`engine_scene::SceneObjectId`]).
    /// Every other row starts folded. Saved with the project's editor
    /// session, so folders stay as the user left them.
    pub(crate) open: BTreeSet<String>,
    /// Objects hidden with the row's eye, by stable object id. Editor only:
    /// saved with the session, never in the scene, so the game ignores it.
    /// [`sync_editor_hidden`] turns it into [`Hidden`] markers.
    pub(crate) hidden: BTreeSet<String>,
    /// The objects Copy last took, for Paste.
    pub(crate) clipboard: Option<ObjectClipboard>,
    /// Set when the user picks a Create menu entry: the command, and the
    /// row it should go under. The shell drains it, as with
    /// [`Self::prefab_request`], because the commands need the whole state.
    pub(crate) create_request: Option<(crate::MenuCommand, Option<Entity>)>,
    /// Whether an image is selected in Project Assets, which Create Sprite
    /// needs. Set by the shell before drawing.
    pub(crate) image_selected: bool,
    /// A file dropped from Project Assets: its path, and the row it should go
    /// under. The shell drains it, as with [`Self::create_request`].
    pub(crate) asset_drop_request: Option<(PathBuf, Option<Entity>)>,
}

impl HierarchyState {
    /// Starts renaming `entity` inline, as F2 does.
    pub(crate) fn start_rename(&mut self, world: &World, entity: Entity) {
        self.renaming = Some(Renaming {
            entity,
            draft: display_name(world, entity),
            focus: true,
        });
        self.reveal = Some(entity);
    }
}

/// `entity`'s parent chain, nearest first (bounded against cycles).
fn ancestors(world: &World, entity: Entity) -> Vec<Entity> {
    let mut chain = Vec::new();
    let mut current = world.get::<ChildOf>(entity).map(ChildOf::parent);
    while let Some(parent) = current {
        if chain.len() as u64 >= u64::from(MAX_HIERARCHY_DEPTH) || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = world.get::<ChildOf>(parent).map(ChildOf::parent);
    }
    chain
}

/// An inline rename in progress.
#[derive(Debug)]
struct Renaming {
    entity: Entity,
    draft: String,
    /// `true` on the first frame, so the text field grabs focus once
    /// without re-grabbing it every frame (which would defeat
    /// `lost_focus` detection).
    focus: bool,
}

/// This world's root entities: named (see below), and not parented to
/// anything else (no `ChildOf`). Saved sibling order is used when present;
/// otherwise entity order keeps the tree stable between frames.
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
    sort_siblings(world, &mut entities);
    entities
}

/// `entity`'s direct children, in saved sibling order when present —
/// empty if it has none (either no [`Children`] component, or one with
/// no entries).
pub fn children_of(world: &World, entity: Entity) -> Vec<Entity> {
    let mut children: Vec<Entity> = world
        .get::<Children>(entity)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();
    sort_siblings(world, &mut children);
    children
}

fn sort_siblings(world: &World, siblings: &mut [Entity]) {
    siblings.sort_by_key(|&entity| {
        let order = world.get::<SiblingOrder>(entity);
        (order.is_none(), order.map_or(0, |order| order.0), entity)
    });
}

fn siblings_of(world: &mut World, parent: Option<Entity>) -> Vec<Entity> {
    match parent {
        Some(parent) => children_of(world, parent),
        None => roots(world),
    }
}

fn set_sibling_order(world: &mut World, siblings: &[Entity]) {
    for (index, &entity) in siblings.iter().enumerate() {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            entity_mut.insert(SiblingOrder(index as u64));
        }
    }
}

fn reparent_at_end(world: &mut World, child: Entity, parent: Option<Entity>) -> bool {
    if !reparent(world, child, parent) {
        return false;
    }
    let mut siblings = siblings_of(world, parent);
    siblings.retain(|&entity| entity != child);
    siblings.push(child);
    set_sibling_order(world, &siblings);
    true
}

/// `entity`'s parent, if it has one.
pub(crate) fn parent_of(world: &World, entity: Entity) -> Option<Entity> {
    world
        .get::<ChildOf>(entity)
        .map(|child_of| child_of.parent())
}

/// Selects `entities`, with the last one primary.
pub(crate) fn select_many(
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    entities: &[Entity],
) {
    if let Some((&last, rest)) = entities.split_last() {
        *selected = Some(last);
        *secondary = rest.to_vec();
    }
}

/// Copies `entities` next to their originals and selects the copies.
pub(crate) fn duplicate_and_select(
    world: &mut World,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    entities: &[Entity],
) -> bool {
    let copies = object_clipboard::duplicate_objects(world, entities);
    select_many(selected, secondary, &copies);
    !copies.is_empty()
}

/// Pastes the clipboard next to the primary selection, or at the scene root
/// with nothing selected, and selects the copies.
pub(crate) fn paste_and_select(
    world: &mut World,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    clipboard: &ObjectClipboard,
) -> bool {
    let after = selected.filter(|&entity| world.get_entity(entity).is_ok());
    let parent = after.and_then(|entity| parent_of(world, entity));
    let copies = object_clipboard::paste_objects(world, clipboard, parent, after);
    select_many(selected, secondary, &copies);
    !copies.is_empty()
}

/// Makes the new object `child` a child of `parent`, keeping its local
/// transform, so it appears at its parent rather than at the world origin.
pub(crate) fn adopt_new_object(world: &mut World, child: Entity, parent: Entity) {
    let local = world.get::<TransformComponent>(child).copied();
    if reparent_at_end(world, child, Some(parent))
        && let Some(local) = local
        && let Ok(mut entity_mut) = world.get_entity_mut(child)
    {
        entity_mut.insert(local);
    }
}

/// Gives every object the [`Hidden`] marker that `hidden` calls for: those
/// listed, and everything under them. Undo rebuilds the world and drops the
/// markers, so this runs every frame. [`Hidden`] is not saved in scenes.
pub fn sync_editor_hidden(world: &mut World, hidden: &BTreeSet<String>) {
    let mut query = world.query::<(
        Entity,
        Option<&engine_scene::SceneObjectId>,
        bevy_ecs::query::Has<Hidden>,
    )>();
    let objects: Vec<(Entity, bool, bool)> = query
        .iter(world)
        .map(|(entity, id, marked)| {
            let listed = id.is_some_and(|id| hidden.contains(&id.0));
            (entity, listed, marked)
        })
        .collect();
    let listed: HashSet<Entity> = objects
        .iter()
        .filter(|(_, listed, _)| *listed)
        .map(|(entity, ..)| *entity)
        .collect();
    for (entity, own, marked) in objects {
        if listed.is_empty() && !marked {
            continue;
        }
        let wanted = own
            || ancestors(world, entity)
                .iter()
                .any(|ancestor| listed.contains(ancestor));
        if wanted != marked
            && let Ok(mut entity_mut) = world.get_entity_mut(entity)
        {
            if wanted {
                entity_mut.insert(Hidden);
            } else {
                entity_mut.remove::<Hidden>();
            }
        }
    }
}

/// The selection if `entity` is part of it, otherwise `entity` alone: what a
/// row's menu or eye acts on.
fn row_targets(entity: Entity, selected: Option<Entity>, secondary: &[Entity]) -> Vec<Entity> {
    if selected == Some(entity) || secondary.contains(&entity) {
        selection_list(selected, secondary)
    } else {
        vec![entity]
    }
}

/// Hides `entities` in the editor, or shows them again.
fn set_editor_hidden(
    world: &mut World,
    hidden: &mut BTreeSet<String>,
    entities: &[Entity],
    hide: bool,
) {
    for &entity in entities {
        if let Some(id) = object_clipboard::ensure_object_id(world, entity) {
            if hide {
                hidden.insert(id);
            } else {
                hidden.remove(&id);
            }
        }
    }
}

/// The Create entries, shared by the `+` menu and the right-click menus. A
/// new object goes under `parent`, or at the scene root.
fn create_menu_items(ui: &mut egui::Ui, state: &mut HierarchyState, parent: Option<Entity>) {
    use crate::MenuCommand as Command;
    for (label, command) in [
        ("Empty", Command::CreateEmpty),
        ("Camera", Command::CreateCamera),
        ("Point Light", Command::CreatePointLight),
        ("Sound", Command::CreateSound),
    ] {
        if ui.button(label).clicked() {
            state.create_request = Some((command, parent));
            ui.close();
        }
    }
    if ui
        .add_enabled(state.image_selected, egui::Button::new("Sprite"))
        .on_disabled_hover_text("Select an image in Project Assets first")
        .clicked()
    {
        state.create_request = Some((Command::CreateSprite, parent));
        ui.close();
    }
    for (label, command) in [
        ("UI Label", Command::CreateUiLabel),
        ("UI Button", Command::CreateUiButton),
    ] {
        if ui.button(label).clicked() {
            state.create_request = Some((command, parent));
            ui.close();
        }
    }
}

/// `entities` without any whose parent is also listed, in tree order.
pub(crate) fn top_objects(world: &mut World, entities: &[Entity]) -> Vec<Entity> {
    let tops = without_descendants(world, entities);
    tree_order(world)
        .into_iter()
        .filter(|entity| tops.contains(entity))
        .collect()
}

/// Puts `objects`, already children of `parent`, together in the sibling
/// list directly after `after`, or last when `after` is not a sibling.
pub(crate) fn place_among_siblings(
    world: &mut World,
    objects: &[Entity],
    parent: Option<Entity>,
    after: Option<Entity>,
) {
    let mut siblings = siblings_of(world, parent);
    siblings.retain(|entity| !objects.contains(entity));
    let index = after
        .and_then(|after| siblings.iter().position(|&entity| entity == after))
        .map_or(siblings.len(), |index| index + 1);
    siblings.splice(index..index, objects.iter().copied());
    set_sibling_order(world, &siblings);
}

/// What a drop would move and the parent it would move to: the selection if
/// `dragged` is part of it, otherwise `dragged` alone, leaving out locked
/// objects and any whose parent also moves. A `target` of `None` with
/// [`DropPlacement::Into`] means the scene root. `None` if the drop is not
/// allowed: `dragged` is locked, the target is one of the moved objects, or
/// the destination is inside one.
fn drop_plan(
    world: &mut World,
    dragged: Entity,
    selection: &[Entity],
    placement: DropPlacement,
    target: Option<Entity>,
) -> Option<(Vec<Entity>, Option<Entity>)> {
    if world.get_entity(dragged).is_err() || world.get::<Lock>(dragged).is_some() {
        return None;
    }
    let listed = if selection.contains(&dragged) {
        selection.to_vec()
    } else {
        vec![dragged]
    };
    let mut moving = top_objects(world, &listed);
    moving.retain(|&entity| world.get::<Lock>(entity).is_none());
    let parent = match placement {
        DropPlacement::Into => target,
        DropPlacement::Before | DropPlacement::After => {
            target.and_then(|target| parent_of(world, target))
        }
    };
    let refused = moving.is_empty()
        || target.is_some_and(|target| moving.contains(&target))
        || parent.is_some_and(|parent| {
            moving
                .iter()
                .any(|&entity| is_descendant(world, parent, entity))
        });
    (!refused).then_some((moving, parent))
}

/// Carries out [`drop_plan`], keeping the moved objects in tree order and
/// where they are in the world. Returns whether anything changed.
fn drop_objects(
    world: &mut World,
    dragged: Entity,
    selection: &[Entity],
    placement: DropPlacement,
    target: Option<Entity>,
) -> bool {
    let Some((moving, parent)) = drop_plan(world, dragged, selection, placement, target) else {
        return false;
    };
    let before = siblings_of(world, parent);
    let mut reparented = false;
    for &entity in &moving {
        if parent_of(world, entity) != parent && reparent(world, entity, parent) {
            reparented = true;
        }
    }
    let mut siblings = siblings_of(world, parent);
    siblings.retain(|entity| !moving.contains(entity));
    let index = match (placement, target) {
        (DropPlacement::Into, _) | (_, None) => siblings.len(),
        (DropPlacement::Before | DropPlacement::After, Some(target)) => {
            let Some(index) = siblings.iter().position(|&entity| entity == target) else {
                return reparented;
            };
            index + usize::from(matches!(placement, DropPlacement::After))
        }
    };
    siblings.splice(index..index, moving.iter().copied());
    if !reparented && siblings == before {
        return false;
    }
    set_sibling_order(world, &siblings);
    true
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

/// The name a new group gets.
pub const GROUP_NAME: &str = "Group";

/// Whether `entity` is a group: an object with children and nothing of its
/// own to show or run — only a name, a place, and the hierarchy, scene and
/// editor bookkeeping. The Hierarchy draws it as a folder, and only a group
/// can be ungrouped. Anything else (a mesh, a light, a script…) makes it an
/// ordinary object that happens to have children.
pub fn is_group(world: &World, entity: Entity) -> bool {
    let bookkeeping = [
        TypeId::of::<Name>(),
        TypeId::of::<TransformComponent>(),
        TypeId::of::<GlobalTransform>(),
        TypeId::of::<ChildOf>(),
        TypeId::of::<Children>(),
        TypeId::of::<SiblingOrder>(),
        TypeId::of::<engine_scene::SceneObjectId>(),
        TypeId::of::<Disabled>(),
        TypeId::of::<Lock>(),
        TypeId::of::<Static>(),
        TypeId::of::<Hidden>(),
    ];
    !children_of(world, entity).is_empty()
        && world.inspect_entity(entity).is_ok_and(|mut components| {
            components.all(|info| info.type_id().is_some_and(|id| bookkeeping.contains(&id)))
        })
}

/// Every entity in the tree, parents before children, in sibling order.
fn tree_order(world: &mut World) -> Vec<Entity> {
    let mut order = Vec::new();
    for root in roots(world) {
        flatten_visible(world, root, None, &mut order);
    }
    order
}

/// Puts `entities` (that are not locked) into a new, empty [`GROUP_NAME`]
/// object at their centre, and returns the group. Nothing moves on screen.
///
/// An entity whose parent is also being grouped stays inside that parent.
/// When every entity shares one parent, the group goes under it, where the
/// first of them was; otherwise it becomes a root. Returns `None` when
/// there is nothing to group.
pub fn group_entities(world: &mut World, entities: &[Entity]) -> Option<Entity> {
    let chosen: HashSet<Entity> = entities
        .iter()
        .copied()
        .filter(|&entity| world.get_entity(entity).is_ok() && world.get::<Lock>(entity).is_none())
        .collect();
    let members: Vec<Entity> = tree_order(world)
        .into_iter()
        .filter(|entity| chosen.contains(entity))
        .filter(|&entity| {
            !ancestors(world, entity)
                .iter()
                .any(|ancestor| chosen.contains(ancestor))
        })
        .collect();
    let first = *members.first()?;
    let parent_of = |world: &World, entity| world.get::<ChildOf>(entity).map(ChildOf::parent);
    let first_parent = parent_of(world, first);
    let parent = members
        .iter()
        .all(|&member| parent_of(world, member) == first_parent)
        .then_some(first_parent)
        .flatten();

    let centre = members
        .iter()
        .map(|&member| world_transform(world, member).translation)
        .sum::<Vec3>()
        / members.len() as f32;
    let group = spawn_entity(world, GROUP_NAME, centre);
    if parent.is_some() && !reparent(world, group, parent) {
        despawn_entity(world, group);
        return None;
    }
    // The group takes the first member's place among its new siblings.
    let mut siblings = siblings_of(world, parent);
    siblings.retain(|&entity| entity != group);
    let at = siblings
        .iter()
        .position(|&entity| entity == first)
        .unwrap_or(siblings.len());
    siblings.insert(at, group);
    for &member in &members {
        reparent(world, member, Some(group));
    }
    siblings.retain(|entity| !members.contains(entity));
    set_sibling_order(world, &siblings);
    set_sibling_order(world, &members);
    Some(group)
}

/// Moves a group's children out to the group's own parent, in its place
/// and order, then deletes the empty group. Nothing moves on screen.
/// Returns the freed children, or `None` if `group` is not an unlocked
/// [`is_group`].
pub fn ungroup(world: &mut World, group: Entity) -> Option<Vec<Entity>> {
    if !is_group(world, group) || world.get::<Lock>(group).is_some() {
        return None;
    }
    let parent = world.get::<ChildOf>(group).map(ChildOf::parent);
    let children = children_of(world, group);
    let mut siblings = siblings_of(world, parent);
    let at = siblings
        .iter()
        .position(|&entity| entity == group)
        .unwrap_or(siblings.len());
    for &child in &children {
        reparent(world, child, parent);
    }
    siblings.retain(|&entity| entity != group);
    for (offset, &child) in children.iter().enumerate() {
        siblings.insert((at + offset).min(siblings.len()), child);
    }
    set_sibling_order(world, &siblings);
    despawn_entity(world, group);
    Some(children)
}

/// Despawns `entity` and its descendants — `bevy_ecs` recurses into
/// `Children`. A harmless no-op if `entity` doesn't exist (e.g. it was
/// already deleted, or the caller's `selected` was stale).
pub fn despawn_entity(world: &mut World, entity: Entity) {
    world.despawn(entity);
}

/// Whether `entity` carries the [`Disabled`] marker.
pub fn is_disabled(world: &World, entity: Entity) -> bool {
    world.get::<Disabled>(entity).is_some()
}

/// Adds or removes the [`Disabled`] marker on `entity`.
fn set_disabled(world: &mut World, entity: Entity, disabled: bool) {
    if disabled {
        world.entity_mut(entity).insert(Disabled);
    } else {
        world.entity_mut(entity).remove::<Disabled>();
    }
}

/// Sets `entity`'s [`Name`] to `name`, inserting the component if it
/// had none.
fn rename(world: &mut World, entity: Entity, name: &str) {
    match world.get_mut::<Name>(entity) {
        Some(mut current) => current.0 = name.to_owned(),
        None => {
            world.entity_mut(entity).insert(Name::new(name));
        }
    }
}

/// Deep-copies `entity` and its subtree, appending " Copy" to the new
/// root's name, and returns the new root. Carries the editor's
/// round-tripped component set (`Name`, `Transform`, `Camera`,
/// `AssetSource`, `Disabled`) — the same one `engine_scene` saves.
pub fn duplicate_entity(world: &mut World, entity: Entity) -> Option<Entity> {
    crate::object_clipboard::duplicate_objects(world, &[entity])
        .first()
        .copied()
}

/// `entity`'s world-space transform: its local [`TransformComponent`]
/// (identity if absent) composed with every ancestor's, walking
/// `ChildOf` up to the root. Depth-bounded.
pub fn world_transform(world: &World, entity: Entity) -> MathTransform {
    let mut chain = Vec::new();
    let mut current = Some(entity);
    for _ in 0..MAX_HIERARCHY_DEPTH {
        let Some(node) = current else { break };
        chain.push(node);
        current = world.get::<ChildOf>(node).map(|child_of| child_of.parent());
    }

    let mut result = MathTransform::IDENTITY;
    for node in chain.into_iter().rev() {
        let local = world
            .get::<TransformComponent>(node)
            .map(|transform| transform.0)
            .unwrap_or(MathTransform::IDENTITY);
        result = result.mul_transform(&local);
    }
    result
}

/// Whether `node` is `ancestor` itself, or sits below it in the
/// `ChildOf` tree — i.e. re-parenting `ancestor` under `node` would form
/// a cycle. Depth-bounded.
pub fn is_descendant(world: &World, node: Entity, ancestor: Entity) -> bool {
    let mut current = Some(node);
    for _ in 0..MAX_HIERARCHY_DEPTH {
        let Some(entity) = current else { break };
        if entity == ancestor {
            return true;
        }
        current = world
            .get::<ChildOf>(entity)
            .map(|child_of| child_of.parent());
    }
    false
}

/// Re-parents `child` under `new_parent` (or to the scene root when
/// `None`), rewriting `child`'s local [`TransformComponent`] so its
/// world-space transform is unchanged by the move.
///
/// Returns `false` without touching the world if the move is illegal:
/// `new_parent` is `child` itself or a descendant of `child` (would
/// cycle), or either entity no longer exists.
pub fn reparent(world: &mut World, child: Entity, new_parent: Option<Entity>) -> bool {
    if world.get_entity(child).is_err() {
        return false;
    }
    if let Some(parent) = new_parent
        && (world.get_entity(parent).is_err() || is_descendant(world, parent, child))
    {
        return false;
    }

    let old_world = world_transform(world, child);
    let parent_world = new_parent.map_or(MathTransform::IDENTITY, |parent| {
        world_transform(world, parent)
    });
    let new_local = parent_world.inverse().mul_transform(&old_world);

    let Ok(mut entity_mut) = world.get_entity_mut(child) else {
        return false;
    };
    match new_parent {
        Some(parent) => {
            entity_mut.insert(ChildOf(parent));
        }
        None => {
            entity_mut.remove::<ChildOf>();
        }
    }
    entity_mut.insert(TransformComponent::from(new_local));
    true
}

/// Gives `entity` the world-space transform `target`, by setting the
/// local transform that produces it under its parent (the inverse of
/// [`world_transform`]). Edits made in world space, like a gizmo drag,
/// then land correctly for an object inside a moved, turned or scaled
/// group.
pub fn set_world_transform(world: &mut World, entity: Entity, target: MathTransform) {
    let parent_world = world
        .get::<ChildOf>(entity)
        .map_or(MathTransform::IDENTITY, |child_of| {
            world_transform(world, child_of.parent())
        });
    let local = parent_world.inverse().mul_transform(&target);
    if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
        entity_mut.insert(TransformComponent::from(local));
    }
}

/// `entities` without any whose parent, grandparent… is also listed:
/// moving a group already moves what is inside it.
pub fn without_descendants(world: &World, entities: &[Entity]) -> Vec<Entity> {
    entities
        .iter()
        .copied()
        .filter(|&entity| {
            !ancestors(world, entity)
                .iter()
                .any(|ancestor| entities.contains(ancestor))
        })
        .collect()
}

/// Every entity in the selection: the `primary` (if any) first, then the
/// `secondary` set.
pub fn selection_list(primary: Option<Entity>, secondary: &[Entity]) -> Vec<Entity> {
    let mut all = Vec::with_capacity(secondary.len() + 1);
    all.extend(primary);
    all.extend_from_slice(secondary);
    all
}

/// Makes `entity` the sole selection: the primary, with `secondary`
/// emptied.
pub fn select_only(primary: &mut Option<Entity>, secondary: &mut Vec<Entity>, entity: Entity) {
    *primary = Some(entity);
    secondary.clear();
}

/// Selects the contiguous run of `order` between the current `primary`
/// (the anchor) and `entity` — `primary` stays the anchor, `secondary`
/// becomes every other row in the run. Falls back to
/// [`select_only`] if there is no anchor, or either end isn't in
/// `order`.
pub fn select_range(
    primary: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    order: &[Entity],
    entity: Entity,
) {
    let anchor = match *primary {
        Some(anchor) => anchor,
        None => {
            select_only(primary, secondary, entity);
            return;
        }
    };
    let (Some(a), Some(b)) = (
        order.iter().position(|&e| e == anchor),
        order.iter().position(|&e| e == entity),
    ) else {
        select_only(primary, secondary, entity);
        return;
    };
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    secondary.clear();
    secondary.extend(order[lo..=hi].iter().copied().filter(|&e| e != anchor));
}

/// Toggles `entity` in the `primary` + `secondary` selection:
/// - already in `secondary` → removed;
/// - the current `primary` → the first `secondary` entry is promoted (or
///   the selection becomes empty);
/// - otherwise → appended to `secondary`.
pub fn select_toggle(primary: &mut Option<Entity>, secondary: &mut Vec<Entity>, entity: Entity) {
    if let Some(index) = secondary.iter().position(|&member| member == entity) {
        secondary.remove(index);
    } else if *primary == Some(entity) {
        *primary = if secondary.is_empty() {
            None
        } else {
            Some(secondary.remove(0))
        };
    } else {
        secondary.push(entity);
    }
}

/// Fills `out` with every entity in `entity`'s subtree (including
/// `entity`) that either matches `query` itself or has a descendant that
/// does. Returns whether `entity`'s subtree matched at all.
fn collect_matches(world: &World, entity: Entity, query: &str, out: &mut HashSet<Entity>) -> bool {
    let mut matched = display_name(world, entity).to_lowercase().contains(query);
    for child in children_of(world, entity) {
        if collect_matches(world, child, query, out) {
            matched = true;
        }
    }
    if matched {
        out.insert(entity);
    }
    matched
}

/// Draws the hierarchy panel into `ui`: a toolbar (Create / Duplicate /
/// Delete), a search box, then the entity tree.
///
/// Every row is selectable at any depth (a manual header row splits the
/// expand triangle from the name), double-click or the right-click menu
/// renames inline, the menu also duplicates / enables-disables /
/// deletes, and a non-empty search shows only branches that contain a
/// match (kept expanded). Selection is written through `*selected`, the
/// same value the Inspector and the Scene-view gizmo read; world edits
/// are turned into undo steps and a dirty flag by
/// [`crate::EditorShell::run_frame`]'s end-of-frame history settle.
pub fn show(
    ui: &mut egui::Ui,
    world: &mut World,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) {
    crate::shell::panel_header(ui, |ui| {
        crate::shell::paint_shell_icon(ui, crate::shell::ShellIcon::Layers);
        ui.label(crate::theme::semibold("Hierarchy"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            ui.menu_button("+", |ui| {
                crate::theme::style_context_menu(ui);
                create_menu_items(ui, state, None);
                ui.separator();
                let selection = selection_list(*selected, secondary);
                let has_selection = !selection.is_empty();
                if ui
                    .add_enabled(has_selection, egui::Button::new("Duplicate   ⌘D"))
                    .clicked()
                {
                    duplicate_and_select(world, selected, secondary, &selection);
                    ui.close();
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Delete"))
                    .clicked()
                {
                    for entity in &selection {
                        despawn_entity(world, *entity);
                    }
                    *selected = None;
                    secondary.clear();
                    state.renaming = None;
                    ui.close();
                }
                // Lock / unlock the whole selection. The label follows the
                // primary's state so a second click undoes the first.
                let primary_locked = selected.is_some_and(|e| world.get::<Lock>(e).is_some());
                if ui
                    .add_enabled(
                        has_selection,
                        egui::Button::new(if primary_locked { "Unlock" } else { "Lock" }),
                    )
                    .on_hover_text(
                        "Locked entities ignore the gizmo, batch move, and reparent-drag",
                    )
                    .clicked()
                {
                    for entity in &selection {
                        if let Ok(mut entity_mut) = world.get_entity_mut(*entity) {
                            if primary_locked {
                                entity_mut.remove::<Lock>();
                            } else {
                                entity_mut.insert(Lock);
                            }
                        }
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("Group Selection   ⌘G"))
                    .on_hover_text("Put the selected objects into a new group, like a folder")
                    .clicked()
                {
                    group_selection(world, selected, secondary, state);
                    ui.close();
                }
                let any_group = selection.iter().any(|&entity| is_group(world, entity));
                if ui
                    .add_enabled(any_group, egui::Button::new("Ungroup   ⇧⌘G"))
                    .on_hover_text("Move the group's objects out and remove the empty group")
                    .clicked()
                {
                    ungroup_selection(world, selected, secondary);
                    ui.close();
                }
                // Prefabs are single-entity: acts on the primary only.
                if ui
                    .add_enabled(selected.is_some(), egui::Button::new("Make Prefab"))
                    .on_hover_text("Save the selected entity as a reusable .prefab")
                    .clicked()
                    && let Some(entity) = *selected
                {
                    state.prefab_request = Some(entity);
                    ui.close();
                }
            });
        });
    });
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(9, 6))
        .show(ui, |ui| {
            egui::Frame::NONE
                .fill(crate::theme::NIGHT.shell)
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(9, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        paint_search_icon(ui);
                        ui.add(
                            egui::TextEdit::singleline(&mut state.search)
                                .hint_text("Filter hierarchy…")
                                .frame(egui::Frame::NONE)
                                .margin(egui::Margin::ZERO)
                                .desired_width(f32::INFINITY),
                        );
                    });
                });

            // Drop target for un-parenting: a row dragged here becomes a root.
            let root_strip = ui.add(
                egui::Label::new(egui::RichText::new("\u{25b8} Scene root").weak())
                    .sense(egui::Sense::click()),
            );
            let selection = selection_list(*selected, secondary);
            if let Some(path) = root_strip.dnd_release_payload::<PathBuf>() {
                state.asset_drop_request = Some(((*path).clone(), None));
            } else if let Some(dragged) = root_strip.dnd_release_payload::<Entity>()
                && drop_objects(world, *dragged, &selection, DropPlacement::Into, None)
            {
                if !selection.contains(&dragged) {
                    select_only(selected, secondary, *dragged);
                }
            } else if root_strip.dnd_hover_payload::<Entity>().is_some()
                || root_strip.dnd_hover_payload::<PathBuf>().is_some()
            {
                ui.painter().rect_filled(
                    root_strip.rect,
                    2.0,
                    ui.visuals().selection.bg_fill.linear_multiply(0.3),
                );
            }
            ui.add_space(4.0);

            let query = state.search.trim().to_lowercase();
            let visible: Option<HashSet<Entity>> = if query.is_empty() {
                None
            } else {
                let mut set = HashSet::new();
                for root in roots(world) {
                    collect_matches(world, root, &query, &mut set);
                }
                Some(set)
            };

            // A selection made elsewhere (the Scene View) is revealed once:
            // its parents expand and its row scrolls into view.
            if *selected != state.last_selected {
                state.last_selected = *selected;
                state.reveal = *selected;
                state.reveal_ancestors =
                    selected.map_or_else(Vec::new, |entity| ancestors(world, entity));
            }

            // Rebuild the flat visible order (drives shift-click range select).
            state.flat_order.clear();
            for root in roots(world) {
                flatten_visible(world, root, visible.as_ref(), &mut state.flat_order);
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let root_entities = roots(world);
                    for &entity in &root_entities {
                        show_entity(
                            ui,
                            world,
                            entity,
                            selected,
                            secondary,
                            state,
                            visible.as_ref(),
                        );
                    }
                    if let Some(&last) = root_entities.last() {
                        end_drop_target(ui, world, last, selected, secondary, state);
                    }
                    // The empty space below the rows: right-click to create
                    // at the scene root or paste.
                    let (_, empty) = ui.allocate_at_least(
                        egui::vec2(ui.available_width(), ui.available_height().max(40.0)),
                        egui::Sense::click(),
                    );
                    if let Some(path) = empty.dnd_release_payload::<PathBuf>() {
                        state.asset_drop_request = Some(((*path).clone(), None));
                    }
                    empty.context_menu(|ui| {
                        crate::theme::style_context_menu(ui);
                        ui.menu_button("Create", |ui| {
                            crate::theme::style_context_menu(ui);
                            create_menu_items(ui, state, None);
                        });
                        let clipboard = state.clipboard.clone();
                        if ui
                            .add_enabled(clipboard.is_some(), egui::Button::new("Paste   ⌘V"))
                            .clicked()
                            && let Some(clipboard) = clipboard
                        {
                            *selected = None;
                            secondary.clear();
                            paste_and_select(world, selected, secondary, &clipboard);
                            ui.close();
                        }
                    });
                });
            state.reveal = None;
            state.reveal_ancestors.clear();
            paint_drag_count(ui, *selected, secondary);
        });
}

/// While several objects are being dragged, says how many beside the pointer:
/// only the row under the pointer is drawn moving.
fn paint_drag_count(ui: &egui::Ui, selected: Option<Entity>, secondary: &[Entity]) {
    let Some(dragged) = egui::DragAndDrop::payload::<Entity>(ui.ctx()) else {
        return;
    };
    let count = row_targets(*dragged, selected, secondary).len();
    if count < 2 {
        return;
    }
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    let palette = crate::theme::NIGHT;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("hierarchy_drag_count"),
    ));
    let galley = painter.layout_no_wrap(
        format!("{count} objects"),
        egui::TextStyle::Small.resolve(ui.style()),
        palette.text,
    );
    let rect = egui::Rect::from_min_size(
        pointer + egui::vec2(14.0, 14.0),
        galley.size() + egui::vec2(12.0, 6.0),
    );
    painter.rect_filled(rect, 6.0, palette.accent_fill);
    painter.galley(rect.min + egui::vec2(6.0, 3.0), galley, palette.text);
}

/// Appends `entity` and its visible descendants to `out` in draw order
/// — the same visibility rule [`show_entity`] uses.
fn flatten_visible(
    world: &World,
    entity: Entity,
    visible: Option<&HashSet<Entity>>,
    out: &mut Vec<Entity>,
) {
    if visible.is_some_and(|set| !set.contains(&entity)) {
        return;
    }
    out.push(entity);
    for child in children_of(world, entity) {
        if visible.is_none_or(|set| set.contains(&child)) {
            flatten_visible(world, child, visible, out);
        }
    }
}

fn show_entity(
    ui: &mut egui::Ui,
    world: &mut World,
    entity: Entity,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
    visible: Option<&HashSet<Entity>>,
) {
    if let Some(set) = visible
        && !set.contains(&entity)
    {
        return;
    }

    let children: Vec<Entity> = children_of(world, entity)
        .into_iter()
        .filter(|child| visible.is_none_or(|set| set.contains(child)))
        .collect();

    let highlight = ui.painter().add(egui::Shape::Noop);
    let in_selection = *selected == Some(entity) || secondary.contains(&entity);
    if children.is_empty() {
        let row = ui.horizontal(|ui| {
            ui.set_min_width(ui.available_width());
            ui.add_space(22.0);
            entity_row(ui, world, entity, selected, secondary, state);
        });
        let drop_into = entity_drop_target(
            ui,
            world,
            entity,
            row.response.rect,
            selected,
            secondary,
            state,
        );
        paint_hierarchy_row(ui, highlight, row.response.rect, in_selection || drop_into);
        return;
    }

    // Folded unless the user unfolded it (remembered by object id), a
    // selection inside it is being revealed, or a search is showing its
    // matches. A search only opens rows for as long as it lasts.
    let key = world
        .get::<engine_scene::SceneObjectId>(entity)
        .map(|id| id.0.clone());
    if state.reveal_ancestors.contains(&entity)
        && let Some(key) = &key
    {
        state.open.insert(key.clone());
    }
    let remembered = key.as_ref().is_some_and(|key| state.open.contains(key));
    let searching = visible.is_some();
    let id = match &key {
        Some(key) => ui.make_persistent_id(("hierarchy_row", key)),
        None => ui.make_persistent_id(("hierarchy_row", entity)),
    };
    let mut collapsing =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, remembered);
    // A row with no id yet (new since the last save or undo step) keeps
    // egui's own in-session fold state until it gets one.
    let revealing = state.reveal_ancestors.contains(&entity);
    if key.is_some() || searching {
        collapsing.set_open(remembered || searching);
    } else if revealing {
        collapsing.set_open(true);
    }
    let header = ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add_space(4.0);
        crate::assets::tree_disclosure(ui, &mut collapsing);
        entity_row(ui, world, entity, selected, secondary, state);
    });
    if !searching && let Some(key) = key {
        if collapsing.is_open() {
            state.open.insert(key);
        } else {
            state.open.remove(&key);
        }
    }
    let drop_into = entity_drop_target(
        ui,
        world,
        entity,
        header.response.rect,
        selected,
        secondary,
        state,
    );
    paint_hierarchy_row(
        ui,
        highlight,
        header.response.rect,
        in_selection || drop_into,
    );
    collapsing.show_body_indented(&header.response, ui, |ui| {
        for &child in &children {
            show_entity(ui, world, child, selected, secondary, state, visible);
        }
        if let Some(&last) = children.last() {
            end_drop_target(ui, world, last, selected, secondary, state);
        }
    });
}

/// One place on a row that a drag can be dropped.
struct DropZone {
    target: Entity,
    hit_rect: egui::Rect,
    placement: DropPlacement,
}

#[derive(Clone, Copy)]
enum DropPlacement {
    Before,
    Into,
    After,
}

fn entity_drop_target(
    ui: &mut egui::Ui,
    world: &mut World,
    target: Entity,
    rect: egui::Rect,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) -> bool {
    let hit_rect = egui::Rect::from_min_max(rect.min - egui::vec2(0.0, 4.0), rect.max);
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let placement = if pointer.is_some_and(|pointer| pointer.y < rect.top()) {
        DropPlacement::Before
    } else {
        DropPlacement::Into
    };
    let zone = DropZone {
        target,
        hit_rect,
        placement,
    };
    handle_entity_drop(ui, world, zone, selected, secondary, state)
}

fn end_drop_target(
    ui: &mut egui::Ui,
    world: &mut World,
    target: Entity,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
    let zone = DropZone {
        target,
        hit_rect: rect,
        placement: DropPlacement::After,
    };
    handle_entity_drop(ui, world, zone, selected, secondary, state);
}

fn handle_entity_drop(
    ui: &mut egui::Ui,
    world: &mut World,
    zone: DropZone,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) -> bool {
    let DropZone {
        target,
        hit_rect,
        placement,
    } = zone;
    let response = ui.interact(
        hit_rect,
        ui.make_persistent_id((
            "hierarchy_drop",
            target,
            matches!(placement, DropPlacement::After),
        )),
        egui::Sense::hover(),
    );
    // A file from Project Assets becomes a child of the row. The gaps between
    // rows ignore files.
    if response.dnd_hover_payload::<PathBuf>().is_some() {
        if !matches!(placement, DropPlacement::Into) {
            return false;
        }
        if let Some(path) = response.dnd_release_payload::<PathBuf>() {
            state.asset_drop_request = Some(((*path).clone(), Some(target)));
            return false;
        }
        return true;
    }
    let Some(dragged) = response.dnd_hover_payload::<Entity>() else {
        return false;
    };
    let dragged = *dragged;
    let selection = selection_list(*selected, secondary);
    if drop_plan(world, dragged, &selection, placement, Some(target)).is_none() {
        return false;
    }
    if response.dnd_release_payload::<Entity>().is_some() {
        if drop_objects(world, dragged, &selection, placement, Some(target))
            && !selection.contains(&dragged)
        {
            select_only(selected, secondary, dragged);
        }
        return false;
    }

    match placement {
        DropPlacement::Before | DropPlacement::After => {
            let line_y = if matches!(placement, DropPlacement::Before) {
                hit_rect.top() + 2.0
            } else {
                hit_rect.center().y
            };
            ui.painter().line_segment(
                [
                    egui::pos2(hit_rect.left() + 4.0, line_y),
                    egui::pos2(hit_rect.right() - 6.0, line_y),
                ],
                egui::Stroke::new(2.0, crate::theme::NIGHT.accent),
            );
            false
        }
        DropPlacement::Into => true,
    }
}

fn paint_hierarchy_row(
    ui: &egui::Ui,
    shape: egui::layers::ShapeIdx,
    rect: egui::Rect,
    selected: bool,
) {
    let fill = if selected {
        Some(crate::theme::NIGHT.accent_fill)
    } else if ui.rect_contains_pointer(rect) {
        Some(crate::theme::NIGHT.hover)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter()
            .set(shape, egui::Shape::rect_filled(rect, 6.0, fill));
    }
}

fn paint_search_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let center = rect.min + egui::vec2(6.0, 6.0);
    let stroke = egui::Stroke::new(1.3, crate::theme::NIGHT.muted);
    ui.painter().circle_stroke(center, 4.0, stroke);
    ui.painter().line_segment(
        [center + egui::vec2(3.0, 3.0), center + egui::vec2(6.0, 6.0)],
        stroke,
    );
}

/// The symbol at the start of a Hierarchy row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowIcon {
    Camera,
    Light,
    Sound,
    /// An on-screen label or button.
    Ui,
    Tilemap,
    Water,
    Sprite,
    Terrain,
    Model,
    /// A folder: an [`is_group`] object.
    Group,
    Object,
}

/// Whether `path` looks like a terrain mesh: a `.gltf` in a `terrain` folder,
/// where [`crate::terrain_tools::write_new_terrain`] creates them. Terrain has
/// no component of its own, and this runs per row per frame, so the `.terrain`
/// file beside the mesh is not checked.
fn is_terrain_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    path.extension()
        .is_some_and(|extension| extension == "gltf")
        && path
            .parent()
            .is_some_and(|parent| parent.ends_with("terrain"))
}

/// The icon for `entity`'s row. An object that is several things takes the
/// first match, with the Scene View's camera > light > sound order.
fn row_icon(world: &World, entity: Entity) -> RowIcon {
    let Ok(object) = world.get_entity(entity) else {
        return RowIcon::Object;
    };
    if object.contains::<Camera>() {
        RowIcon::Camera
    } else if object.contains::<engine_scene::ScenePointLight>() {
        RowIcon::Light
    } else if object.contains::<engine_scene::SceneAudioEmitter>() {
        RowIcon::Sound
    } else if object.contains::<engine_scene::SceneUi>() {
        RowIcon::Ui
    } else if object.contains::<engine_scene::SceneTilemap>() {
        RowIcon::Tilemap
    } else if object.contains::<WaterVolume>() {
        RowIcon::Water
    } else if object.contains::<Sprite>() || object.contains::<SpriteSource>() {
        RowIcon::Sprite
    } else if object
        .get::<AssetSource>()
        .is_some_and(|source| is_terrain_path(&source.path))
    {
        RowIcon::Terrain
    } else if object.contains::<AssetSource>()
        || object.contains::<MeshSource>()
        || object.contains::<MeshRenderer>()
    {
        RowIcon::Model
    } else if is_group(world, entity) {
        RowIcon::Group
    } else {
        RowIcon::Object
    }
}

/// Groups the panel's selection (the panel-side twin of
/// [`crate::EditorState::group_selection`]).
fn group_selection(
    world: &mut World,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) {
    if let Some(group) = group_entities(world, &selection_list(*selected, secondary)) {
        select_only(selected, secondary, group);
        state.start_rename(world, group);
    }
}

/// Ungroups the panel's selection (the panel-side twin of
/// [`crate::EditorState::ungroup_selection`]).
fn ungroup_selection(
    world: &mut World,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
) {
    let freed: Vec<Entity> = selection_list(*selected, secondary)
        .into_iter()
        .filter_map(|entity| ungroup(world, entity))
        .flatten()
        .collect();
    if let Some((&first, rest)) = freed.split_first() {
        *selected = Some(first);
        *secondary = rest.to_vec();
    }
}

fn paint_entity_icon(ui: &mut egui::Ui, icon: RowIcon) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    let ink = crate::theme::NIGHT.muted;
    let stroke = egui::Stroke::new(1.2, ink);
    let painter = ui.painter();
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    let glyph = |kind| {
        crate::scene_icons::paint_glyph(painter, kind, rect.center(), 0.75, ink, ink);
    };
    match icon {
        RowIcon::Camera => glyph(crate::scene_icons::IconKind::Camera),
        RowIcon::Light => glyph(crate::scene_icons::IconKind::Light),
        RowIcon::Sound => glyph(crate::scene_icons::IconKind::Sound),
        RowIcon::Ui => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(1.0, 3.5), at(13.0, 10.5)),
                2.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            line(&[(4.0, 7.0), (10.0, 7.0)]);
        }
        RowIcon::Tilemap => {
            line(&[
                (2.0, 2.0),
                (12.0, 2.0),
                (12.0, 12.0),
                (2.0, 12.0),
                (2.0, 2.0),
            ]);
            line(&[(7.0, 2.0), (7.0, 12.0)]);
            line(&[(2.0, 7.0), (12.0, 7.0)]);
        }
        RowIcon::Water => {
            for y in [5.0, 9.5] {
                line(&[
                    (1.0, y),
                    (3.0, y - 1.5),
                    (5.0, y),
                    (7.0, y + 1.5),
                    (9.0, y),
                    (11.0, y - 1.5),
                    (13.0, y),
                ]);
            }
        }
        RowIcon::Sprite => {
            line(&[
                (1.0, 2.5),
                (13.0, 2.5),
                (13.0, 11.5),
                (1.0, 11.5),
                (1.0, 2.5),
            ]);
            line(&[
                (1.0, 10.0),
                (5.0, 6.0),
                (8.0, 9.0),
                (10.0, 7.5),
                (13.0, 10.0),
            ]);
        }
        RowIcon::Terrain => {
            line(&[(1.0, 9.0), (4.5, 4.5), (7.0, 7.0), (9.5, 3.0), (13.0, 9.0)]);
            line(&[(1.0, 12.0), (13.0, 12.0)]);
        }
        RowIcon::Model => {
            line(&[
                (7.0, 1.0),
                (13.0, 4.0),
                (13.0, 10.0),
                (7.0, 13.0),
                (1.0, 10.0),
                (1.0, 4.0),
                (7.0, 1.0),
            ]);
            line(&[(1.0, 4.0), (7.0, 7.0)]);
            line(&[(13.0, 4.0), (7.0, 7.0)]);
            line(&[(7.0, 7.0), (7.0, 13.0)]);
        }
        RowIcon::Group => {
            line(&[
                (1.0, 3.0),
                (5.5, 3.0),
                (7.0, 4.5),
                (13.0, 4.5),
                (13.0, 12.0),
                (1.0, 12.0),
                (1.0, 3.0),
            ]);
            line(&[(1.0, 6.5), (13.0, 6.5)]);
        }
        RowIcon::Object => {
            line(&[(7.0, 3.5), (10.5, 7.0), (7.0, 10.5), (3.5, 7.0), (7.0, 3.5)]);
        }
    }
}

/// Width kept free at the end of a row for the eye.
const EYE_SLOT_WIDTH: f32 = 22.0;

/// The show/hide eye, slashed when `hidden`. Fainter when it is `inherited`
/// from a hidden parent, and brighter under the pointer.
fn paint_eye_icon(ui: &egui::Ui, rect: egui::Rect, hidden: bool, inherited: bool, hovered: bool) {
    let palette = crate::theme::NIGHT;
    let ink = if hovered {
        palette.text
    } else if inherited {
        palette.subtle.gamma_multiply(0.6)
    } else if hidden {
        palette.subtle
    } else {
        palette.muted
    };
    let stroke = egui::Stroke::new(1.2, ink);
    let at = rect.center();
    let point = |x: f32, y: f32| at + egui::vec2(x, y);
    let painter = ui.painter();
    painter.add(egui::Shape::line(
        vec![
            point(-6.5, 0.0),
            point(-3.5, -3.0),
            point(0.0, -4.0),
            point(3.5, -3.0),
            point(6.5, 0.0),
            point(3.5, 3.0),
            point(0.0, 4.0),
            point(-3.5, 3.0),
            point(-6.5, 0.0),
        ],
        stroke,
    ));
    painter.circle_filled(at, 1.8, ink);
    if hidden {
        painter.line_segment([point(-5.5, 5.0), point(5.5, -5.0)], stroke);
    }
}

fn paint_lock_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.2, crate::theme::NIGHT.subtle);
    ui.painter().add(egui::Shape::line(
        vec![
            at(4.0, 6.0),
            at(4.0, 4.0),
            at(5.5, 2.0),
            at(8.5, 2.0),
            at(10.0, 4.0),
            at(10.0, 6.0),
        ],
        stroke,
    ));
    ui.painter().add(egui::Shape::line(
        vec![
            at(3.0, 6.0),
            at(11.0, 6.0),
            at(11.0, 12.0),
            at(3.0, 12.0),
            at(3.0, 6.0),
        ],
        stroke,
    ));
}

/// The name portion of a hierarchy row: an inline rename field when this
/// entity is being renamed, otherwise a selectable label plus its
/// right-click menu.
fn entity_row(
    ui: &mut egui::Ui,
    world: &mut World,
    entity: Entity,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
    state: &mut HierarchyState,
) {
    if state.renaming.as_ref().map(|r| r.entity) == Some(entity) {
        let Some(mut renaming) = state.renaming.take() else {
            return;
        };
        let response =
            ui.add(egui::TextEdit::singleline(&mut renaming.draft).desired_width(f32::INFINITY));
        if renaming.focus {
            response.request_focus();
            renaming.focus = false;
        }
        let enter = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        if enter {
            let name = renaming.draft.trim();
            if !name.is_empty() {
                rename(world, entity, name);
            }
        } else if escape || response.lost_focus() {
            // Discard — `renaming` was already taken, so it stays cleared.
        } else {
            state.renaming = Some(renaming);
        }
        return;
    }

    let disabled = is_disabled(world, entity);
    let locked = world.get::<Lock>(entity).is_some();
    let name = display_name(world, entity);
    let hidden_itself = world
        .get::<engine_scene::SceneObjectId>(entity)
        .is_some_and(|id| state.hidden.contains(&id.0));
    let hidden = hidden_itself || world.get::<Hidden>(entity).is_some();
    let text = if disabled {
        egui::RichText::new(name)
            .color(crate::theme::NIGHT.subtle)
            .italics()
    } else if hidden {
        egui::RichText::new(name).color(crate::theme::NIGHT.subtle)
    } else if *selected == Some(entity) || secondary.contains(&entity) {
        egui::RichText::new(name).color(crate::theme::NIGHT.text)
    } else {
        egui::RichText::new(name).color(crate::theme::NIGHT.muted)
    };

    let row_id = egui::Id::new(("hierarchy_dnd", entity));
    let row_rect = ui
        .dnd_drag_source(row_id, entity, |ui| {
            let row_rect =
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 27.0));
            ui.horizontal(|ui| {
                paint_entity_icon(ui, row_icon(world, entity));
                let reserved = EYE_SLOT_WIDTH + if locked { 20.0 } else { 0.0 };
                ui.allocate_ui_with_layout(
                    egui::vec2((ui.available_width() - reserved).max(0.0), 27.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(text)
                                .truncate()
                                .sense(egui::Sense::click()),
                        );
                    },
                );
                if locked {
                    paint_lock_icon(ui);
                }
            });
            row_rect
        })
        .inner;
    let eye_rect = egui::Rect::from_center_size(
        egui::pos2(row_rect.right() - EYE_SLOT_WIDTH / 2.0, row_rect.center().y),
        egui::vec2(EYE_SLOT_WIDTH - 6.0, 14.0),
    );
    let response = ui.interact(row_rect, row_id, egui::Sense::click_and_drag());
    if state.reveal == Some(entity) {
        response.scroll_to_me(Some(egui::Align::Center));
    }
    // After the row, so a click on the eye toggles instead of selecting.
    let eye = ui.interact(eye_rect, row_id.with("eye"), egui::Sense::click());
    if hidden || response.hovered() || eye.hovered() {
        paint_eye_icon(
            ui,
            eye_rect,
            hidden,
            hidden && !hidden_itself,
            eye.hovered(),
        );
    }
    if eye
        .on_hover_text(if hidden_itself {
            "Show in the Scene View"
        } else {
            "Hide in the Scene View (the game is not affected)"
        })
        .clicked()
    {
        let targets = row_targets(entity, *selected, secondary);
        set_editor_hidden(world, &mut state.hidden, &targets, !hidden_itself);
    }
    if response.clicked() {
        let (multi, range) =
            ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
        if range {
            select_range(selected, secondary, &state.flat_order, entity);
        } else if multi {
            select_toggle(selected, secondary, entity);
        } else {
            select_only(selected, secondary, entity);
        }
    }
    // Editor convention: double-click frames the object; F2 renames it.
    if response.double_clicked() {
        select_only(selected, secondary, entity);
        state.frame_request = true;
    }
    if *selected == Some(entity)
        && state.renaming.is_none()
        && ui.input(|input| input.key_pressed(egui::Key::F2))
        && !ui.ctx().text_edit_focused()
    {
        state.renaming = Some(Renaming {
            entity,
            draft: display_name(world, entity),
            focus: true,
        });
    }

    response.context_menu(|ui| {
        crate::theme::style_context_menu(ui);
        ui.menu_button("Create", |ui| {
            crate::theme::style_context_menu(ui);
            create_menu_items(ui, state, Some(entity));
        });
        ui.separator();
        if ui.button("Rename").clicked() {
            state.renaming = Some(Renaming {
                entity,
                draft: display_name(world, entity),
                focus: true,
            });
            ui.close();
        }
        let targets = row_targets(entity, *selected, secondary);
        if ui.button("Duplicate   ⌘D").clicked() {
            duplicate_and_select(world, selected, secondary, &targets);
            ui.close();
        }
        if ui.button("Copy   ⌘C").clicked() {
            state.clipboard = object_clipboard::copy_objects(world, &targets);
            ui.close();
        }
        let clipboard = state.clipboard.clone();
        if ui
            .add_enabled(clipboard.is_some(), egui::Button::new("Paste   ⌘V"))
            .clicked()
            && let Some(clipboard) = clipboard
        {
            select_only(selected, secondary, entity);
            paste_and_select(world, selected, secondary, &clipboard);
            ui.close();
        }
        // Group acts on the whole selection when this row is part of it,
        // otherwise on this row alone.
        if ui.button("Group").clicked() {
            if *selected != Some(entity) && !secondary.contains(&entity) {
                select_only(selected, secondary, entity);
            }
            group_selection(world, selected, secondary, state);
            ui.close();
        }
        if is_group(world, entity) && !locked && ui.button("Ungroup").clicked() {
            if *selected != Some(entity) && !secondary.contains(&entity) {
                select_only(selected, secondary, entity);
            }
            ungroup_selection(world, selected, secondary);
            ui.close();
        }
        if ui.button("Make Prefab").clicked() {
            state.prefab_request = Some(entity);
            ui.close();
        }
        if ui
            .button(if disabled { "Enable" } else { "Disable" })
            .clicked()
        {
            set_disabled(world, entity, !disabled);
            ui.close();
        }
        if ui
            .button(if locked { "Unlock" } else { "Lock" })
            .on_hover_text("A locked object cannot be moved or clicked in the Scene View")
            .clicked()
        {
            if locked {
                world.entity_mut(entity).remove::<Lock>();
            } else {
                world.entity_mut(entity).insert(Lock);
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Delete").clicked() {
            despawn_entity(world, entity);
            if *selected == Some(entity) {
                *selected = None;
            }
            secondary.retain(|&member| member != entity);
            state.renaming = None;
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_list_the_parent_chain_nearest_first() {
        let mut world = World::new();
        let root = world.spawn_empty().id();
        let middle = world.spawn(ChildOf(root)).id();
        let leaf = world.spawn(ChildOf(middle)).id();
        assert_eq!(ancestors(&world, leaf), vec![middle, root]);
        assert!(ancestors(&world, root).is_empty());
    }
    use engine_ecs::prelude::ChildOf;

    #[test]
    fn hierarchy_header_uses_vector_icon_not_a_text_glyph() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let text_glyph = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text() == "▦",
            _ => false,
        });
        output.textures_delta.clear();
        assert!(!text_glyph);
    }

    #[test]
    fn add_object_button_has_space_from_header_edge() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                ..Default::default()
            },
            |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
        );
        let plus_right = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "+" => {
                Some(text.pos.x + text.galley.size().x)
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            plus_right.is_some_and(|right| 320.0 - right >= 18.0),
            "add icon is too close to the card edge: {plus_right:?}"
        );
    }

    #[test]
    fn locked_entity_uses_a_vector_indicator_not_an_emoji() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        world.spawn((Name::new("Locked"), Lock));
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let emoji = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text().contains('🔒'),
            _ => false,
        });
        output.textures_delta.clear();
        assert!(!emoji);
    }

    #[test]
    fn terrain_paths_are_gltf_files_in_a_terrain_folder() {
        assert!(is_terrain_path("terrain/Terrain.gltf"));
        assert!(is_terrain_path("terrain/Terrain 2.gltf"));
        assert!(!is_terrain_path("props/barrel.gltf"));
        assert!(!is_terrain_path("terrain/notes.txt"));
        assert!(!is_terrain_path("terrain.gltf"));
    }

    fn test_camera() -> Camera {
        Camera(engine_renderer::Camera::new(Vec3::ONE, Vec3::ZERO, 1.0))
    }

    /// One object of every row kind, in the order [`RowIcon`] lists them, for
    /// the icon tests and the snapshot.
    fn one_object_of_each_kind(world: &mut World) -> Vec<(Entity, RowIcon)> {
        let no_asset = || engine_scene::AssetRef { id: String::new() };
        let camera = spawn_entity(world, "Camera", Vec3::ZERO);
        world.entity_mut(camera).insert(test_camera());
        let light = spawn_entity(world, "Point Light", Vec3::ZERO);
        world
            .entity_mut(light)
            .insert(engine_scene::ScenePointLight(
                engine_scene::PointLightData::default(),
            ));
        let sound = spawn_entity(world, "Sound", Vec3::ZERO);
        world
            .entity_mut(sound)
            .insert(engine_scene::SceneAudioEmitter(
                engine_scene::AudioEmitterData::new(no_asset()),
            ));
        let ui = spawn_entity(world, "Start Button", Vec3::ZERO);
        world
            .entity_mut(ui)
            .insert(engine_scene::SceneUi(engine_scene::UiElementData::button()));
        let tilemap = spawn_entity(world, "Level", Vec3::ZERO);
        world.entity_mut(tilemap).insert(engine_scene::SceneTilemap(
            engine_scene::TilemapData::new(no_asset()),
        ));
        let water = spawn_entity(world, "Lake", Vec3::ZERO);
        world.entity_mut(water).insert(WaterVolume {
            surface: engine_physics::WaterSurface::default(),
        });
        let sprite = spawn_entity(world, "Player Sprite", Vec3::ZERO);
        world.entity_mut(sprite).insert(SpriteSource {
            atlas: String::new(),
            region: "full".into(),
            size: [1.0, 1.0],
            color: [1.0; 4],
            z_order: 0.0,
        });
        let terrain = spawn_entity(world, "Terrain", Vec3::ZERO);
        world
            .entity_mut(terrain)
            .insert(AssetSource::new("terrain/Terrain.gltf"));
        let model = spawn_entity(world, "Barrel", Vec3::ZERO);
        world
            .entity_mut(model)
            .insert(AssetSource::new("props/barrel.gltf"));
        let group = spawn_entity(world, "Trees", Vec3::ZERO);
        let leaf = spawn_entity(world, "Tree", Vec3::ZERO);
        reparent(world, leaf, Some(group));
        let object = spawn_entity(world, "Empty", Vec3::ZERO);
        vec![
            (camera, RowIcon::Camera),
            (light, RowIcon::Light),
            (sound, RowIcon::Sound),
            (ui, RowIcon::Ui),
            (tilemap, RowIcon::Tilemap),
            (water, RowIcon::Water),
            (sprite, RowIcon::Sprite),
            (terrain, RowIcon::Terrain),
            (model, RowIcon::Model),
            (group, RowIcon::Group),
            (object, RowIcon::Object),
        ]
    }

    #[test]
    fn every_kind_of_object_gets_its_own_row_icon() {
        let mut world = World::new();
        for (entity, expected) in one_object_of_each_kind(&mut world) {
            assert_eq!(
                row_icon(&world, entity),
                expected,
                "{}",
                display_name(&world, entity)
            );
        }
    }

    #[test]
    fn an_object_that_is_several_things_takes_the_scene_view_order() {
        let mut world = World::new();
        let both = spawn_entity(&mut world, "Both", Vec3::ZERO);
        world.entity_mut(both).insert((
            engine_scene::ScenePointLight(engine_scene::PointLightData::default()),
            test_camera(),
        ));
        assert_eq!(row_icon(&world, both), RowIcon::Camera);

        // A model with children is still a model, never a folder.
        let parent = spawn_entity(&mut world, "Crate", Vec3::ZERO);
        world
            .entity_mut(parent)
            .insert(AssetSource::new("props/crate.gltf"));
        let child = spawn_entity(&mut world, "Lid", Vec3::ZERO);
        reparent(&mut world, child, Some(parent));
        assert_eq!(row_icon(&world, parent), RowIcon::Model);
    }

    /// Three roots, plus a `Folder` holding `Inner`.
    fn drag_fixture(world: &mut World) -> [Entity; 5] {
        let [a, b, c, folder, inner] =
            ["A", "B", "C", "Folder", "Inner"].map(|name| spawn_entity(world, name, Vec3::ZERO));
        reparent(world, inner, Some(folder));
        set_sibling_order(world, &[a, b, c, folder]);
        [a, b, c, folder, inner]
    }

    #[test]
    fn dragging_several_rows_moves_them_together_in_order() {
        let mut world = World::new();
        let [a, b, c, folder, inner] = drag_fixture(&mut world);

        // Selection order does not matter: they keep the tree's order.
        let selection = [c, a];
        assert!(drop_objects(
            &mut world,
            a,
            &selection,
            DropPlacement::Before,
            Some(inner)
        ));
        assert_eq!(children_of(&world, folder), vec![a, c, inner]);
        assert_eq!(roots(&mut world), vec![b, folder]);

        assert!(drop_objects(
            &mut world,
            c,
            &[a, c],
            DropPlacement::After,
            Some(inner)
        ));
        assert_eq!(children_of(&world, folder), vec![inner, a, c]);

        assert!(drop_objects(
            &mut world,
            a,
            &[a, c],
            DropPlacement::Into,
            None
        ));
        assert_eq!(roots(&mut world), vec![b, folder, a, c]);

        assert!(drop_objects(
            &mut world,
            a,
            &[a, b],
            DropPlacement::Into,
            Some(folder)
        ));
        assert_eq!(children_of(&world, folder), vec![inner, b, a]);
    }

    #[test]
    fn a_row_outside_the_selection_drags_alone() {
        let mut world = World::new();
        let [a, b, c, folder, _] = drag_fixture(&mut world);
        assert!(drop_objects(
            &mut world,
            c,
            &[a, b],
            DropPlacement::Into,
            Some(folder)
        ));
        assert_eq!(roots(&mut world), vec![a, b, folder]);
    }

    #[test]
    fn a_drop_onto_or_inside_a_moved_object_is_refused() {
        let mut world = World::new();
        let [a, b, _, folder, inner] = drag_fixture(&mut world);
        for (target, placement) in [
            (b, DropPlacement::Before),
            (folder, DropPlacement::Into),
            (inner, DropPlacement::Into),
            (inner, DropPlacement::After),
        ] {
            assert!(!drop_objects(
                &mut world,
                a,
                &[a, b, folder],
                placement,
                Some(target)
            ));
        }
        assert_eq!(children_of(&world, folder), vec![inner]);
    }

    #[test]
    fn locked_objects_stay_behind_a_multi_drag() {
        let mut world = World::new();
        let [a, b, _, folder, inner] = drag_fixture(&mut world);
        world.entity_mut(b).insert(Lock);
        assert!(drop_objects(
            &mut world,
            a,
            &[a, b],
            DropPlacement::Into,
            Some(folder)
        ));
        assert_eq!(children_of(&world, folder), vec![inner, a]);
        assert!(parent_of(&world, b).is_none());
        assert!(!drop_objects(
            &mut world,
            b,
            &[b],
            DropPlacement::Into,
            Some(folder)
        ));
    }

    #[test]
    fn a_multi_drag_keeps_world_positions() {
        let mut world = World::new();
        let folder = spawn_entity(&mut world, "Folder", Vec3::new(10.0, 0.0, 0.0));
        let a = spawn_entity(&mut world, "A", Vec3::new(1.0, 0.0, 0.0));
        let b = spawn_entity(&mut world, "B", Vec3::new(2.0, 0.0, 0.0));
        assert!(drop_objects(
            &mut world,
            a,
            &[a, b],
            DropPlacement::Into,
            Some(folder)
        ));
        assert!(
            world_transform(&world, b)
                .translation
                .abs_diff_eq(Vec3::new(2.0, 0.0, 0.0), 1e-5)
        );
    }

    #[test]
    fn hiding_marks_children_survives_a_rebuild_and_never_touches_the_scene() {
        let mut world = World::new();
        let parent = spawn_entity(&mut world, "Parent", Vec3::ZERO);
        let child = spawn_entity(&mut world, "Child", Vec3::ZERO);
        let other = spawn_entity(&mut world, "Other", Vec3::ZERO);
        reparent(&mut world, child, Some(parent));
        let mut hidden = BTreeSet::new();
        let before = engine_scene::Scene::from_world(&mut world);

        set_editor_hidden(&mut world, &mut hidden, &[parent], true);
        sync_editor_hidden(&mut world, &hidden);
        assert!(world.get::<Hidden>(parent).is_some());
        assert!(world.get::<Hidden>(child).is_some(), "children follow");
        assert!(world.get::<Hidden>(other).is_none());
        assert_eq!(
            engine_scene::Scene::from_world(&mut world),
            before,
            "the scene is unchanged"
        );

        // Undo rebuilds the world from a scene, dropping every marker.
        let mut rebuilt = World::new();
        let spawned = before.instantiate(&mut rebuilt);
        assert!(spawned.iter().all(|&e| rebuilt.get::<Hidden>(e).is_none()));
        sync_editor_hidden(&mut rebuilt, &hidden);
        assert_eq!(
            spawned
                .iter()
                .filter(|&&e| rebuilt.get::<Hidden>(e).is_some())
                .count(),
            2
        );

        set_editor_hidden(&mut world, &mut hidden, &[parent], false);
        sync_editor_hidden(&mut world, &hidden);
        assert!(world.get::<Hidden>(parent).is_none());
        assert!(world.get::<Hidden>(child).is_none());
    }

    #[test]
    fn a_hidden_group_is_still_a_group() {
        let mut world = World::new();
        let group = spawn_entity(&mut world, "Trees", Vec3::ZERO);
        let tree = spawn_entity(&mut world, "Tree", Vec3::ZERO);
        reparent(&mut world, tree, Some(group));
        let mut hidden = BTreeSet::new();
        set_editor_hidden(&mut world, &mut hidden, &[group], true);
        sync_editor_hidden(&mut world, &hidden);
        assert!(is_group(&world, group));
        assert_eq!(row_icon(&world, group), RowIcon::Group);
    }

    #[test]
    fn hiding_an_object_with_no_id_gives_it_one() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("New")).id();
        let mut hidden = BTreeSet::new();
        set_editor_hidden(&mut world, &mut hidden, &[entity], true);
        sync_editor_hidden(&mut world, &hidden);
        assert_eq!(hidden.len(), 1);
        assert!(world.get::<Hidden>(entity).is_some());
    }

    #[test]
    fn hidden_objects_cannot_be_clicked_in_the_scene_view() {
        let mut world = World::new();
        let light = spawn_entity(&mut world, "Light", Vec3::ZERO);
        world
            .entity_mut(light)
            .insert(engine_scene::ScenePointLight(
                engine_scene::PointLightData::default(),
            ));
        assert_eq!(crate::scene_icons::collect(&mut world).len(), 1);
        world.entity_mut(light).insert(Hidden);
        assert!(crate::scene_icons::collect(&mut world).is_empty());
    }

    #[test]
    fn a_new_child_keeps_its_local_transform() {
        let mut world = World::new();
        let parent = spawn_entity(&mut world, "Parent", Vec3::new(10.0, 0.0, 0.0));
        let light = spawn_entity(&mut world, "Light", Vec3::new(0.0, 2.0, 0.0));
        adopt_new_object(&mut world, light, parent);
        assert_eq!(parent_of(&world, light), Some(parent));
        assert!(
            world_transform(&world, light)
                .translation
                .abs_diff_eq(Vec3::new(10.0, 2.0, 0.0), 1e-5),
            "it appears at its parent"
        );
    }

    #[test]
    #[ignore = "needs a GPU; writes hierarchy-icons.png"]
    fn snapshot_hierarchy_icons() {
        let mut world = World::new();
        let objects = one_object_of_each_kind(&mut world);
        let mut state = HierarchyState::default();
        // A multi-selection, one hidden object and one hidden parent.
        let (mut selected, mut secondary) = (Some(objects[0].0), vec![objects[1].0]);
        let by_icon = |icon| objects.iter().find(|(_, kind)| *kind == icon).unwrap().0;
        set_editor_hidden(
            &mut world,
            &mut state.hidden,
            &[by_icon(RowIcon::Water), by_icon(RowIcon::Group)],
            true,
        );
        sync_editor_hidden(&mut world, &state.hidden);
        let path = crate::ui_snapshot::snapshot_dir().join("hierarchy-icons.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(280.0, 480.0), 2.0, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show(ui, &mut world, &mut selected, &mut secondary, &mut state);
            });
        })
        .expect("snapshot");
    }

    #[test]
    fn hierarchy_entity_names_align_next_to_their_icons() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        world.spawn(Name::new("Camera"));
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let x = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Camera" => Some(text.pos.x),
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            x.is_some_and(|x| x < 100.0),
            "name must start beside its icon, got {x:?}"
        );
    }

    #[test]
    fn clicking_a_hierarchy_row_still_selects_its_entity() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let entity = world.spawn(Name::new("Camera")).id();
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Camera" => {
                Some(text.pos + egui::vec2(120.0, 8.0))
            }
            _ => None,
        });
        output.textures_delta.clear();
        let pos = pos.unwrap();
        for pressed in [true, false] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            output.textures_delta.clear();
        }
        assert_eq!(selected, Some(entity));
    }

    #[test]
    fn dragging_a_hierarchy_row_does_not_crash() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let entity = world.spawn(Name::new("Dragged")).id();
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let row = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Dragged" => {
                Some(text.pos + egui::vec2(5.0, 8.0))
            }
            _ => None,
        });
        output.textures_delta.clear();
        let row = row.expect("hierarchy row");
        for (time, events) in [
            (
                0.1,
                vec![
                    egui::Event::PointerMoved(row),
                    egui::Event::PointerButton {
                        pos: row,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ),
            (
                0.2,
                vec![egui::Event::PointerMoved(row + egui::vec2(30.0, 0.0))],
            ),
            (
                0.3,
                vec![egui::Event::PointerMoved(row + egui::vec2(40.0, 0.0))],
            ),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            output.textures_delta.clear();
        }
        assert_eq!(
            egui::DragAndDrop::payload::<Entity>(&ctx).as_deref(),
            Some(&entity)
        );
    }

    #[test]
    fn dragging_a_hierarchy_row_onto_another_reparents_it() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let dragged = spawn_entity(&mut world, "Dragged", Vec3::ZERO);
        let target = spawn_entity(&mut world, "Target", Vec3::ZERO);
        world.spawn((Name::new("Existing child"), ChildOf(target)));
        let mut selected = Some(target);
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let row = |name: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == name => {
                    Some(text.pos + egui::vec2(5.0, 8.0))
                }
                _ => None,
            })
        };
        let from = row("Dragged").expect("dragged row");
        let target_label = row("Target").expect("target row");
        let target_row = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.fill == crate::theme::NIGHT.accent_fill
                    && rect.rect.contains(target_label) =>
            {
                Some(rect.rect)
            }
            _ => None,
        });
        let to = egui::pos2(
            target_label.x,
            target_row.expect("target highlight").bottom() - 1.0,
        );
        output.textures_delta.clear();
        for (time, events) in [
            (
                0.1,
                vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton {
                        pos: from,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ),
            (
                0.2,
                vec![egui::Event::PointerMoved(from + egui::vec2(30.0, 0.0))],
            ),
            (0.3, vec![egui::Event::PointerMoved(to)]),
            (
                0.4,
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            if time == 0.3 {
                let drop_highlight = output.shapes.iter().any(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect) => {
                        rect.fill == crate::theme::NIGHT.accent_fill && rect.rect.contains(to)
                    }
                    _ => false,
                });
                assert!(drop_highlight, "row hover must highlight the new parent");
            }
            output.textures_delta.clear();
        }
        assert_eq!(
            world.get::<ChildOf>(dragged).map(|parent| parent.parent()),
            Some(target)
        );
    }

    #[test]
    fn dropping_in_the_gap_above_a_row_reorders_without_reparenting() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        spawn_entity(&mut world, "One", Vec3::ZERO);
        spawn_entity(&mut world, "Two", Vec3::ZERO);
        spawn_entity(&mut world, "Three", Vec3::ZERO);
        let [first, target, dragged] = <[Entity; 3]>::try_from(roots(&mut world)).unwrap();
        let target_name = display_name(&world, target);
        let dragged_name = display_name(&world, dragged);
        let mut selected = Some(target);
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let row = |name: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == name => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let from = row(&dragged_name).unwrap().center();
        let target_text = row(&target_name).unwrap();
        let target_row = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.fill == crate::theme::NIGHT.accent_fill
                    && rect.rect.contains(target_text.center()) =>
            {
                Some(rect.rect)
            }
            _ => None,
        });
        let to = egui::pos2(target_text.center().x, target_row.unwrap().top() - 1.0);
        output.textures_delta.clear();
        for (time, events) in [
            (
                0.1,
                vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton {
                        pos: from,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ),
            (
                0.2,
                vec![egui::Event::PointerMoved(from + egui::vec2(30.0, 0.0))],
            ),
            (0.3, vec![egui::Event::PointerMoved(to)]),
            (
                0.4,
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            if time == 0.3 {
                let line = frame.shapes.iter().any(|shape| match &shape.shape {
                    egui::epaint::Shape::LineSegment { points, stroke } => {
                        stroke.color == crate::theme::NIGHT.accent
                            && stroke.width >= 2.0
                            && (points[0].y - target_row.unwrap().top()).abs() < 4.0
                    }
                    _ => false,
                });
                assert!(line, "gap hover must show an insertion line");
            }
            frame.textures_delta.clear();
        }
        assert!(world.get::<ChildOf>(dragged).is_none());
        assert_eq!(roots(&mut world), vec![first, dragged, target]);
    }

    #[test]
    fn dropping_below_the_last_row_moves_an_entity_to_the_end() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        spawn_entity(&mut world, "One", Vec3::ZERO);
        spawn_entity(&mut world, "Two", Vec3::ZERO);
        spawn_entity(&mut world, "Three", Vec3::ZERO);
        let [dragged, middle, last] = <[Entity; 3]>::try_from(roots(&mut world)).unwrap();
        let dragged_name = display_name(&world, dragged);
        let last_name = display_name(&world, last);
        let mut selected = Some(last);
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let row = |name: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == name => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let from = row(&dragged_name).unwrap().center();
        let last_text = row(&last_name).unwrap();
        let last_row = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.fill == crate::theme::NIGHT.accent_fill
                    && rect.rect.contains(last_text.center()) =>
            {
                Some(rect.rect)
            }
            _ => None,
        });
        let to = egui::pos2(last_text.center().x, last_row.unwrap().bottom() + 5.0);
        output.textures_delta.clear();
        for (time, events) in [
            (
                0.1,
                vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton {
                        pos: from,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ),
            (
                0.2,
                vec![egui::Event::PointerMoved(from + egui::vec2(30.0, 0.0))],
            ),
            (0.3, vec![egui::Event::PointerMoved(to)]),
            (
                0.4,
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            frame.textures_delta.clear();
        }
        assert_eq!(roots(&mut world), vec![middle, last, dragged]);
    }

    #[test]
    fn dropping_a_file_on_a_row_asks_for_a_child_of_that_row() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let row = spawn_entity(&mut world, "Target", Vec3::ZERO);
        let (mut selected, mut secondary) = (None, Vec::new());
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let to = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Target" => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
            .unwrap();
        output.textures_delta.clear();
        let path = PathBuf::from("props/barrel.gltf");
        egui::DragAndDrop::set_payload(&ctx, path.clone());
        for (time, events) in [
            (0.1, vec![egui::Event::PointerMoved(to)]),
            (
                0.2,
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            ),
        ] {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            frame.textures_delta.clear();
        }
        assert_eq!(state.asset_drop_request, Some((path, Some(row))));
    }

    #[test]
    fn reordering_between_parents_keeps_sibling_order_and_world_position() {
        let mut world = World::new();
        let first_parent = spawn_at(&mut world, "First parent", Vec3::new(10.0, 0.0, 0.0));
        let second_parent = spawn_at(&mut world, "Second parent", Vec3::new(20.0, 0.0, 0.0));
        let dragged = child_at(
            &mut world,
            "Dragged",
            Vec3::new(2.0, 0.0, 0.0),
            first_parent,
        );
        let before = child_at(&mut world, "Before", Vec3::ZERO, second_parent);
        let target = child_at(&mut world, "Target", Vec3::ZERO, second_parent);
        set_sibling_order(&mut world, &[before, target]);

        assert!(drop_objects(
            &mut world,
            dragged,
            &[],
            DropPlacement::Before,
            Some(target)
        ));
        assert_eq!(
            children_of(&world, second_parent),
            vec![before, dragged, target]
        );
        assert_eq!(
            world.get::<ChildOf>(dragged).map(|p| p.parent()),
            Some(second_parent)
        );
        assert!((world_transform(&world, dragged).translation.x - 12.0).abs() < 1e-4);

        let scene = engine_scene::Scene::from_world(&mut world);
        let mut loaded = World::new();
        let entities = scene.instantiate(&mut loaded);
        let loaded_parent = entities
            .iter()
            .copied()
            .find(|&entity| display_name(&loaded, entity) == "Second parent")
            .expect("saved parent");
        let names: Vec<_> = children_of(&loaded, loaded_parent)
            .into_iter()
            .map(|entity| display_name(&loaded, entity))
            .collect();
        assert_eq!(names, ["Before", "Dragged", "Target"]);
    }

    #[test]
    fn reordering_before_a_descendant_does_not_create_a_cycle() {
        let mut world = World::new();
        let parent = spawn_at(&mut world, "Parent", Vec3::ZERO);
        let child = child_at(&mut world, "Child", Vec3::ZERO, parent);
        let descendant = child_at(&mut world, "Descendant", Vec3::ZERO, child);

        assert!(!drop_objects(
            &mut world,
            parent,
            &[],
            DropPlacement::Before,
            Some(descendant)
        ));
        assert_eq!(
            world.get::<ChildOf>(child).map(|p| p.parent()),
            Some(parent)
        );
        assert_eq!(
            world.get::<ChildOf>(descendant).map(|p| p.parent()),
            Some(child)
        );
        assert!(world.get::<SiblingOrder>(parent).is_none());
    }

    #[test]
    fn selected_parent_highlight_includes_disclosure_arrow() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        world.spawn((Name::new("Child"), ChildOf(parent)));
        let mut selected = Some(parent);
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let name = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Parent" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        let name = name.unwrap();
        let arrow = egui::pos2(name.left() - 32.0, name.center().y);
        let selected_fill = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => {
                rect.fill == crate::theme::NIGHT.accent_fill
                    && rect.rect.contains(arrow)
                    && rect.rect.contains(name.center())
            }
            _ => false,
        });
        output.textures_delta.clear();
        assert!(selected_fill, "selection must wrap the arrow and label");
    }

    #[test]
    fn rows_start_folded_and_open_when_remembered() {
        let ctx = egui::Context::default();
        let mut world = World::new();
        let trees = world
            .spawn((
                Name::new("Trees"),
                engine_scene::SceneObjectId("trees-id".to_owned()),
            ))
            .id();
        world.spawn((Name::new("Tree 001"), ChildOf(trees)));
        let mut state = HierarchyState::default();
        let (mut selected, mut secondary) = (None, Vec::new());
        let mut time = 0.0;
        let mut shows = |world: &mut World, state: &mut HierarchyState, label: &str| {
            let mut output = egui::FullOutput::default();
            for _ in 0..10 {
                time += 0.1;
                output.textures_delta.clear();
                output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(time),
                        ..Default::default()
                    },
                    |ui| show(ui, world, &mut selected, &mut secondary, state),
                );
            }
            output.textures_delta.clear();
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == label)
            })
        };
        assert!(shows(&mut world, &mut state, "Trees"));
        assert!(
            !shows(&mut world, &mut state, "Tree 001"),
            "groups start folded"
        );
        assert!(state.open.is_empty());

        state.open.insert("trees-id".to_owned());
        assert!(
            shows(&mut world, &mut state, "Tree 001"),
            "a row the user unfolded opens again"
        );
        assert!(state.open.contains("trees-id"), "and stays remembered");
    }

    #[test]
    fn selected_hierarchy_rows_fill_the_available_panel_width() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        let leaf = world.spawn((Name::new("Leaf"), ChildOf(parent))).id();
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut time = 0.0;
        for (entity, label) in [(parent, "Parent"), (leaf, "Leaf")] {
            let mut selected = Some(entity);
            // Several frames: selecting the leaf unfolds its parent, which
            // animates open.
            let mut output = egui::FullOutput::default();
            for _ in 0..10 {
                time += 0.1;
                output.textures_delta.clear();
                output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(360.0, 280.0),
                        )),
                        time: Some(time),
                        ..Default::default()
                    },
                    |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
                );
            }
            let name = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            });
            let name = name.unwrap();
            let selected_right = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == crate::theme::NIGHT.accent_fill && rect.rect.contains(name) =>
                {
                    Some(rect.rect.right())
                }
                _ => None,
            });
            output.textures_delta.clear();
            assert!(
                selected_right.is_some_and(|right| right >= 340.0),
                "{label} highlight ends at {selected_right:?}, leaving panel space unused"
            );
        }
    }

    #[test]
    fn hovering_parent_arrow_highlights_the_whole_row() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut world = World::new();
        let parent = world.spawn(Name::new("Parent")).id();
        world.spawn((Name::new("Child"), ChildOf(parent)));
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &mut world, &mut selected, &mut secondary, &mut state);
        });
        let name = first.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Parent" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        let name = name.unwrap();
        let arrow = egui::pos2(name.left() - 32.0, name.center().y);
        first.textures_delta.clear();
        let mut hovered = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(arrow)],
                ..Default::default()
            },
            |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
        );
        let hover_fill = hovered.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect) => {
                rect.fill == crate::theme::NIGHT.hover
                    && rect.rect.contains(arrow)
                    && rect.rect.contains(name.center())
            }
            _ => false,
        });
        hovered.textures_delta.clear();
        assert!(hover_fill, "hover must wrap the arrow and label");
    }

    #[test]
    fn hierarchy_context_menu_items_have_asset_menu_spacing() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut world = World::new();
        world.spawn(Name::new("Target"));
        let mut selected = None;
        let mut secondary = Vec::new();
        let mut state = HierarchyState::default();
        let mut draw = |time, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(320.0, 280.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            )
        };
        let mut first = draw(0.0, vec![]);
        let text_center = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            })
        };
        let row = text_center(&first, "Target").unwrap();
        first.textures_delta.clear();
        let mut opening = draw(
            0.1,
            vec![
                egui::Event::PointerMoved(row),
                egui::Event::PointerButton {
                    pos: row,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: row,
                    button: egui::PointerButton::Secondary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        opening.textures_delta.clear();
        let mut menu = draw(0.2, vec![]);
        let rename = text_center(&menu, "Rename").unwrap();
        let duplicate = text_center(&menu, "Duplicate   ⌘D").unwrap();
        menu.textures_delta.clear();
        assert!(duplicate.y - rename.y >= 30.0);
    }

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

    #[test]
    fn set_and_query_disabled() {
        let mut world = World::new();
        let entity = spawn_entity(&mut world, "Prop", Vec3::ZERO);
        assert!(!is_disabled(&world, entity));

        set_disabled(&mut world, entity, true);
        assert!(is_disabled(&world, entity));

        set_disabled(&mut world, entity, false);
        assert!(!is_disabled(&world, entity));
    }

    #[test]
    fn rename_sets_or_inserts_the_name() {
        let mut world = World::new();
        let named = world.spawn(Name::new("Old")).id();
        rename(&mut world, named, "New");
        assert_eq!(display_name(&world, named), "New");

        let bare = world.spawn_empty().id();
        rename(&mut world, bare, "Fresh");
        assert_eq!(display_name(&world, bare), "Fresh");
    }

    #[test]
    fn duplicate_keeps_camera_follow_and_remaps_target_inside_subtree() {
        use engine_ecs::components::{ActiveCamera, CameraFollow, FollowMode};
        let mut world = World::new();
        let root = world
            .spawn((Name::new("Rig"), engine_scene::SceneObjectId("rig".into())))
            .id();
        let target = world
            .spawn((
                Name::new("Hero"),
                engine_scene::SceneObjectId("hero".into()),
                ChildOf(root),
            ))
            .id();
        let cam = world
            .spawn((
                Name::new("Cam"),
                CameraFollow::new("hero", FollowMode::THIRD_PERSON),
                ActiveCamera,
                ChildOf(root),
            ))
            .id();
        let _ = (target, cam);

        let copy = duplicate_entity(&mut world, root).unwrap();
        let kids = children_of(&world, copy);
        let copied_cam = kids
            .iter()
            .copied()
            .find(|e| world.get::<CameraFollow>(*e).is_some())
            .expect("follow copied");
        let copied_hero = kids.iter().copied().find(|e| *e != copied_cam).unwrap();
        let new_id = world
            .get::<engine_scene::SceneObjectId>(copied_hero)
            .unwrap()
            .0
            .clone();
        assert_ne!(new_id, "hero");
        assert_eq!(
            world.get::<CameraFollow>(copied_cam).unwrap().target,
            new_id
        );
        assert!(world.get::<ActiveCamera>(copied_cam).is_none());
        // Original untouched.
        assert_eq!(world.get::<CameraFollow>(cam).unwrap().target, "hero");
    }

    #[test]
    fn duplicate_keeps_camera_follow_target_outside_subtree() {
        use engine_ecs::components::{CameraFollow, FollowMode};
        let mut world = World::new();
        let cam = world
            .spawn((
                Name::new("Cam"),
                CameraFollow::new("elsewhere", FollowMode::SIDE_2D),
            ))
            .id();
        let copy = duplicate_entity(&mut world, cam).unwrap();
        assert_eq!(world.get::<CameraFollow>(copy).unwrap().target, "elsewhere");
    }

    #[test]
    fn duplicate_entity_deep_copies_subtree_and_suffixes_the_root() {
        let mut world = World::new();
        let parent = spawn_entity(&mut world, "Parent", Vec3::new(1.0, 0.0, 0.0));
        let child = world
            .spawn((
                Name::new("Child"),
                TransformComponent::from(engine_utils::Transform::from_translation(Vec3::Y)),
                Disabled,
                ChildOf(parent),
            ))
            .id();

        let copy = duplicate_entity(&mut world, parent).unwrap();

        assert_ne!(copy, parent);
        assert_eq!(display_name(&world, copy), "Parent Copy");
        assert_eq!(
            world.get::<TransformComponent>(copy).unwrap().0.translation,
            Vec3::new(1.0, 0.0, 0.0)
        );

        let copied_children = children_of(&world, copy);
        assert_eq!(copied_children.len(), 1);
        let copied_child = copied_children[0];
        assert_ne!(copied_child, child);
        assert_eq!(display_name(&world, copied_child), "Child");
        assert!(is_disabled(&world, copied_child));

        // Original untouched.
        assert_eq!(display_name(&world, parent), "Parent");
        assert_eq!(children_of(&world, parent), vec![child]);
    }

    #[test]
    fn duplicating_tilemap_keeps_grid_and_painted_sprite() {
        let mut world = World::new();
        let id = uuid::Uuid::new_v4().to_string();
        let parent = world
            .spawn((
                Name::new("Tilemap"),
                engine_scene::SceneTilemap(engine_scene::TilemapData::new(
                    engine_scene::AssetRef { id: id.clone() },
                )),
            ))
            .id();
        world.spawn((
            Name::new("Tile (0, 0)"),
            ChildOf(parent),
            engine_ecs::components::SpriteSource {
                atlas: id,
                region: "grid:4,4:1,0".into(),
                size: [1.0, 1.0],
                color: [1.0; 4],
                z_order: 0.0,
            },
        ));

        let copy = duplicate_entity(&mut world, parent).unwrap();
        assert!(world.get::<engine_scene::SceneTilemap>(copy).is_some());
        let copied_child = children_of(&world, copy)[0];
        assert_eq!(
            world
                .get::<engine_ecs::components::SpriteSource>(copied_child)
                .unwrap()
                .region,
            "grid:4,4:1,0"
        );
    }

    #[test]
    fn duplicating_an_animated_sprite_keeps_its_flipbook() {
        let mut world = World::new();
        let data = engine_scene::SpriteAnimationData {
            frames: 4,
            fps: 8.0,
            looping: true,
        };
        let original = world
            .spawn((
                Name::new("Coin"),
                // A scene saves an animation as part of its sprite.
                SpriteSource {
                    atlas: String::new(),
                    region: "full".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                engine_scene::SceneSpriteAnimation::new(data),
            ))
            .id();
        let copy = duplicate_entity(&mut world, original).unwrap();
        assert_eq!(
            world
                .get::<engine_scene::SceneSpriteAnimation>(copy)
                .unwrap()
                .data,
            data
        );
    }

    #[test]
    fn duplicating_a_button_rule_keeps_a_valid_independent_target() {
        let mut world = World::new();
        let original_id = uuid::Uuid::new_v4().to_string();
        let original = world
            .spawn((
                Name::new("Start"),
                engine_scene::SceneObjectId(original_id.clone()),
                engine_scene::SceneUi(engine_scene::UiElementData::button()),
                engine_scene::SceneRules(vec![engine_scene::RuleData {
                    trigger: engine_scene::RuleTrigger::UiClicked,
                    action: engine_scene::RuleAction::SetUiText {
                        target: original_id.clone(),
                        text: "Ready".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: engine_scene::RuleMatch::All,
                    graph_position: None,
                }]),
            ))
            .id();

        let copy = duplicate_entity(&mut world, original).unwrap();
        let copy_id = &world.get::<engine_scene::SceneObjectId>(copy).unwrap().0;
        assert_ne!(copy_id, &original_id);
        assert!(world.get::<engine_scene::SceneUi>(copy).is_some());
        assert!(matches!(
            &world.get::<engine_scene::SceneRules>(copy).unwrap().0[0].action,
            engine_scene::RuleAction::SetUiText { target, .. } if target == copy_id
        ));
        engine_scene::Scene::from_world(&mut world)
            .validate()
            .unwrap();
    }

    #[test]
    fn duplicating_a_rule_subtree_targets_its_copied_child() {
        let mut world = World::new();
        let label_id = uuid::Uuid::new_v4().to_string();
        let button = world
            .spawn((
                Name::new("Start"),
                engine_scene::SceneObjectId(uuid::Uuid::new_v4().to_string()),
                engine_scene::SceneUi(engine_scene::UiElementData::button()),
                engine_scene::SceneRules(vec![engine_scene::RuleData {
                    trigger: engine_scene::RuleTrigger::UiClicked,
                    action: engine_scene::RuleAction::SetUiText {
                        target: label_id.clone(),
                        text: "Ready".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: engine_scene::RuleMatch::All,
                    graph_position: None,
                }]),
            ))
            .id();
        world.spawn((
            Name::new("Status"),
            ChildOf(button),
            engine_scene::SceneObjectId(label_id.clone()),
            engine_scene::SceneUi(engine_scene::UiElementData::label()),
        ));

        let copy = duplicate_entity(&mut world, button).unwrap();
        let child = children_of(&world, copy)[0];
        let copied_label_id = &world.get::<engine_scene::SceneObjectId>(child).unwrap().0;
        assert_ne!(copied_label_id, &label_id);
        assert!(matches!(
            &world.get::<engine_scene::SceneRules>(copy).unwrap().0[0].action,
            engine_scene::RuleAction::SetUiText { target, .. } if target == copied_label_id
        ));
    }

    #[test]
    fn duplicating_remaps_spawn_and_trigger_object_ids() {
        use engine_scene::{RuleAction, RuleData, RuleTrigger, SpawnAt};
        let mut world = World::new();
        let child_id = uuid::Uuid::new_v4().to_string();
        let root = world
            .spawn((
                Name::new("Root"),
                engine_scene::SceneObjectId(uuid::Uuid::new_v4().to_string()),
                engine_scene::SceneRules(vec![RuleData::new(
                    RuleTrigger::TriggerEnter {
                        with: Some(child_id.clone()),
                    },
                    RuleAction::SpawnPrefab {
                        path: "assets/prefabs/a.prefab".into(),
                        at: SpawnAt::Object(child_id.clone()),
                    },
                )]),
            ))
            .id();
        world.spawn((
            Name::new("Child"),
            ChildOf(root),
            engine_scene::SceneObjectId(child_id.clone()),
        ));
        let copy = duplicate_entity(&mut world, root).unwrap();
        let child = children_of(&world, copy)[0];
        let new_id = world
            .get::<engine_scene::SceneObjectId>(child)
            .unwrap()
            .0
            .clone();
        assert_ne!(new_id, child_id);
        let rule = &world.get::<engine_scene::SceneRules>(copy).unwrap().0[0];
        assert!(
            matches!(&rule.trigger, RuleTrigger::TriggerEnter { with: Some(id) } if *id == new_id)
        );
        assert!(
            matches!(&rule.action, RuleAction::SpawnPrefab { at: SpawnAt::Object(id), .. } if *id == new_id)
        );
    }

    #[test]
    fn collect_matches_includes_ancestors_of_a_match() {
        let mut world = World::new();
        let root = world.spawn(Name::new("World")).id();
        let mid = world.spawn((Name::new("Vehicles"), ChildOf(root))).id();
        let leaf = world.spawn((Name::new("Player Car"), ChildOf(mid))).id();
        let other = world.spawn((Name::new("Tree"), ChildOf(root))).id();

        let mut visible = HashSet::new();
        collect_matches(&world, root, "car", &mut visible);

        assert!(visible.contains(&root), "ancestor stays visible");
        assert!(visible.contains(&mid));
        assert!(visible.contains(&leaf), "the match itself");
        assert!(!visible.contains(&other), "non-matching sibling is hidden");
    }

    #[test]
    fn collect_matches_is_case_insensitive() {
        let mut world = World::new();
        let entity = world.spawn(Name::new("MainCamera")).id();
        let mut visible = HashSet::new();
        assert!(collect_matches(&world, entity, "maincamera", &mut visible));
        assert!(visible.contains(&entity));
    }

    #[test]
    fn hierarchy_state_starts_empty() {
        let state = HierarchyState::default();
        assert!(state.search.is_empty());
        assert!(state.renaming.is_none());
        assert!(state.prefab_request.is_none());
        assert!(state.flat_order.is_empty());
    }

    fn spawn_at(world: &mut World, name: &str, translation: Vec3) -> Entity {
        world
            .spawn((
                Name::new(name),
                TransformComponent::from(MathTransform::from_translation(translation)),
            ))
            .id()
    }

    fn child_at(world: &mut World, name: &str, translation: Vec3, parent: Entity) -> Entity {
        world
            .spawn((
                Name::new(name),
                TransformComponent::from(MathTransform::from_translation(translation)),
                ChildOf(parent),
            ))
            .id()
    }

    #[test]
    fn world_transform_composes_the_parent_chain() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::new(10.0, 0.0, 0.0));
        let c = child_at(&mut world, "C", Vec3::new(1.0, 0.0, 0.0), a);
        assert!(
            (world_transform(&world, c).translation - Vec3::new(11.0, 0.0, 0.0)).length() < 1e-4
        );
    }

    #[test]
    fn is_descendant_walks_the_chain() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::ZERO);
        let b = child_at(&mut world, "B", Vec3::ZERO, a);
        let c = child_at(&mut world, "C", Vec3::ZERO, b);
        assert!(is_descendant(&world, c, a));
        assert!(
            is_descendant(&world, a, a),
            "an entity is its own descendant"
        );
        assert!(!is_descendant(&world, a, c));
    }

    #[test]
    fn reparent_preserves_world_position() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::new(10.0, 0.0, 0.0));
        let b = spawn_at(&mut world, "B", Vec3::new(0.0, 5.0, 0.0));
        let c = child_at(&mut world, "C", Vec3::new(1.0, 0.0, 0.0), a);

        assert!(reparent(&mut world, c, Some(b)));
        assert_eq!(world.get::<ChildOf>(c).map(|p| p.parent()), Some(b));
        assert!(
            (world_transform(&world, c).translation - Vec3::new(11.0, 0.0, 0.0)).length() < 1e-3,
            "world position unchanged after the move"
        );
    }

    #[test]
    fn reparent_to_root_keeps_world_position() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::new(10.0, 2.0, 0.0));
        let c = child_at(&mut world, "C", Vec3::new(1.0, 0.0, 0.0), a);

        assert!(reparent(&mut world, c, None));
        assert!(world.get::<ChildOf>(c).is_none());
        assert!(
            (world_transform(&world, c).translation - Vec3::new(11.0, 2.0, 0.0)).length() < 1e-3
        );
    }

    #[test]
    fn reparent_rejects_self_and_descendants_without_mutating() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::ZERO);
        let b = child_at(&mut world, "B", Vec3::ZERO, a);

        assert!(!reparent(&mut world, a, Some(a)), "self-parent rejected");
        assert!(
            !reparent(&mut world, a, Some(b)),
            "parenting under a descendant rejected"
        );
        assert!(world.get::<ChildOf>(a).is_none(), "A untouched");
        assert_eq!(world.get::<ChildOf>(b).map(|p| p.parent()), Some(a));
    }

    #[test]
    fn reparent_on_a_despawned_entity_is_a_harmless_no_op() {
        let mut world = World::new();
        let a = spawn_at(&mut world, "A", Vec3::ZERO);
        let c = spawn_at(&mut world, "C", Vec3::ZERO);
        world.despawn(c);
        assert!(!reparent(&mut world, c, Some(a)));
    }

    fn entities(n: u32) -> Vec<Entity> {
        let mut world = World::new();
        (0..n).map(|_| world.spawn_empty().id()).collect()
    }

    #[test]
    fn select_only_replaces_and_empties_secondary() {
        let e = entities(3);
        let mut primary = Some(e[0]);
        let mut secondary = vec![e[1], e[2]];
        select_only(&mut primary, &mut secondary, e[2]);
        assert_eq!(primary, Some(e[2]));
        assert!(secondary.is_empty());
    }

    #[test]
    fn select_toggle_adds_removes_and_promotes() {
        let e = entities(3);
        let mut primary = Some(e[0]);
        let mut secondary = vec![];

        select_toggle(&mut primary, &mut secondary, e[1]); // add to secondary
        assert_eq!(secondary, vec![e[1]]);

        select_toggle(&mut primary, &mut secondary, e[1]); // remove from secondary
        assert!(secondary.is_empty());

        select_toggle(&mut primary, &mut secondary, e[2]); // secondary = [e2]
        select_toggle(&mut primary, &mut secondary, e[0]); // toggle primary off -> promote e2
        assert_eq!(primary, Some(e[2]));
        assert!(secondary.is_empty());

        select_toggle(&mut primary, &mut secondary, e[2]); // last one -> empty
        assert_eq!(primary, None);
    }

    #[test]
    fn selection_list_is_primary_then_secondary() {
        let e = entities(3);
        assert_eq!(
            selection_list(Some(e[0]), &[e[1], e[2]]),
            vec![e[0], e[1], e[2]]
        );
        assert_eq!(selection_list(None, &[e[1]]), vec![e[1]]);
        assert!(selection_list(None, &[]).is_empty());
    }

    #[test]
    fn select_range_covers_the_run_between_anchor_and_target() {
        let order = entities(5);

        let mut primary = Some(order[1]);
        let mut secondary = vec![];
        select_range(&mut primary, &mut secondary, &order, order[3]);
        assert_eq!(primary, Some(order[1]));
        assert_eq!(secondary, vec![order[2], order[3]]);

        // Target above the anchor works the same.
        let mut primary = Some(order[3]);
        let mut secondary = vec![order[4]];
        select_range(&mut primary, &mut secondary, &order, order[1]);
        assert_eq!(primary, Some(order[3]));
        assert_eq!(secondary, vec![order[1], order[2]]);
    }

    #[test]
    fn select_range_without_an_anchor_falls_back_to_select_only() {
        let order = entities(3);
        let mut primary = None;
        let mut secondary = vec![order[0]];
        select_range(&mut primary, &mut secondary, &order, order[2]);
        assert_eq!(primary, Some(order[2]));
        assert!(secondary.is_empty());
    }

    fn named_at(world: &mut World, name: &str, x: f32) -> Entity {
        spawn_entity(world, name, Vec3::new(x, 0.0, 0.0))
    }

    fn names(world: &World, entities: &[Entity]) -> Vec<String> {
        entities.iter().map(|&e| display_name(world, e)).collect()
    }

    #[test]
    fn grouping_keeps_positions_order_and_place() {
        let mut world = World::new();
        let first = named_at(&mut world, "First", 0.0);
        let tree_a = named_at(&mut world, "Tree A", 2.0);
        let tree_b = named_at(&mut world, "Tree B", 4.0);
        let last = named_at(&mut world, "Last", 9.0);
        set_sibling_order(&mut world, &[first, tree_a, tree_b, last]);

        // Selection order does not matter; the tree's order is kept.
        let group = group_entities(&mut world, &[tree_b, tree_a]).expect("grouped");
        assert_eq!(display_name(&world, group), GROUP_NAME);
        assert_eq!(
            roots(&mut world),
            vec![first, group, last],
            "in the trees' place"
        );
        assert_eq!(children_of(&world, group), vec![tree_a, tree_b]);
        assert_eq!(
            world_transform(&world, group).translation,
            Vec3::new(3.0, 0.0, 0.0),
            "the group sits at their centre"
        );
        for (entity, x) in [(tree_a, 2.0), (tree_b, 4.0)] {
            assert!(
                (world_transform(&world, entity).translation - Vec3::new(x, 0.0, 0.0)).length()
                    < 1e-5,
                "nothing moves on screen"
            );
        }
        assert!(is_group(&world, group));
    }

    #[test]
    fn grouping_skips_locked_and_nested_picks_and_stays_under_a_shared_parent() {
        let mut world = World::new();
        let forest = named_at(&mut world, "Forest", 10.0);
        let tree = named_at(&mut world, "Tree", 12.0);
        let canopy = named_at(&mut world, "Canopy", 12.0);
        let rock = named_at(&mut world, "Rock", 14.0);
        let pinned = named_at(&mut world, "Pinned", 16.0);
        world.entity_mut(pinned).insert(Lock);
        for child in [tree, rock, pinned] {
            reparent_at_end(&mut world, child, Some(forest));
        }
        reparent_at_end(&mut world, canopy, Some(tree));

        let group = group_entities(&mut world, &[tree, canopy, rock, pinned]).expect("grouped");
        assert_eq!(
            world.get::<ChildOf>(group).map(ChildOf::parent),
            Some(forest),
            "all picks shared a parent, so the group goes under it"
        );
        assert_eq!(children_of(&world, group), vec![tree, rock]);
        assert_eq!(
            children_of(&world, tree),
            vec![canopy],
            "a picked child stays with its parent"
        );
        assert_eq!(
            world.get::<ChildOf>(pinned).map(ChildOf::parent),
            Some(forest),
            "a locked object is left alone"
        );
        assert_eq!(group_entities(&mut world, &[pinned]), None);
        assert_eq!(group_entities(&mut world, &[]), None);

        // Picks from different parents make a root group.
        let loose = named_at(&mut world, "Loose", 0.0);
        let mixed = group_entities(&mut world, &[loose, rock]).expect("grouped");
        assert!(world.get::<ChildOf>(mixed).is_none());
    }

    #[test]
    fn only_empty_objects_with_children_are_groups_even_after_saving() {
        let mut world = World::new();
        let folder = named_at(&mut world, "Folder", 0.0);
        let child = named_at(&mut world, "Child", 1.0);
        reparent(&mut world, child, Some(folder));
        let barrel = world
            .spawn((
                Name::new("Barrel"),
                AssetSource::new("props/barrel.gltf"),
                TransformComponent::default(),
            ))
            .id();
        let lid = named_at(&mut world, "Lid", 0.0);
        reparent(&mut world, lid, Some(barrel));
        world.entity_mut(folder).insert((Lock, Disabled));

        assert!(is_group(&world, folder));
        assert!(
            !is_group(&world, barrel),
            "an object with its own content is not a group"
        );
        assert!(!is_group(&world, child), "no children, nothing to ungroup");

        // A saved and reloaded group is still a group.
        let scene = engine_scene::Scene::from_world(&mut world);
        let mut reloaded = World::new();
        scene.instantiate(&mut reloaded);
        let mut query = reloaded.query::<(Entity, &Name)>();
        let folder = query
            .iter(&reloaded)
            .find(|(_, name)| name.0 == "Folder")
            .map(|(entity, _)| entity)
            .expect("folder reloaded");
        assert!(is_group(&reloaded, folder));
    }

    #[test]
    fn ungrouping_puts_the_children_back_in_the_groups_place() {
        let mut world = World::new();
        let first = named_at(&mut world, "First", 0.0);
        let a = named_at(&mut world, "A", 2.0);
        let b = named_at(&mut world, "B", 4.0);
        let last = named_at(&mut world, "Last", 9.0);
        set_sibling_order(&mut world, &[first, a, b, last]);
        let group = group_entities(&mut world, &[a, b]).expect("grouped");
        // Move the group so the children's world positions differ from
        // their local ones; ungrouping must keep them where they are seen.
        world
            .entity_mut(group)
            .insert(TransformComponent::from(MathTransform::from_translation(
                Vec3::new(3.0, 5.0, 0.0),
            )));
        let seen = world_transform(&world, a).translation;

        assert_eq!(ungroup(&mut world, a), None, "not a group");
        assert_eq!(ungroup(&mut world, group), Some(vec![a, b]));
        assert!(
            world.get_entity(group).is_err(),
            "the empty group is removed"
        );
        assert_eq!(
            {
                let order = roots(&mut world);
                names(&world, &order)
            },
            ["First", "A", "B", "Last"]
        );
        assert!((world_transform(&world, a).translation - seen).length() < 1e-5);

        let again = group_entities(&mut world, &[a, b]).expect("grouped");
        world.entity_mut(again).insert(Lock);
        assert_eq!(ungroup(&mut world, again), None, "a locked group stays");
    }
}
