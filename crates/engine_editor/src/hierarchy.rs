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

use std::collections::HashSet;

use engine_ecs::components::{
    AssetSource, Camera, Disabled, Lock, Name, Transform as TransformComponent,
};
use engine_ecs::prelude::{ChildOf, Children, Entity, With, Without, World};
use engine_scene::SiblingOrder;
use engine_utils::Transform as MathTransform;
use glam::Vec3;

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
    /// The visible rows in draw order, rebuilt each [`show`] — the basis
    /// for shift-click range selection.
    pub(crate) flat_order: Vec<Entity>,
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

fn reorder_relative(world: &mut World, dragged: Entity, target: Entity, after: bool) -> bool {
    if dragged == target
        || world.get_entity(dragged).is_err()
        || world.get_entity(target).is_err()
        || world.get::<Lock>(dragged).is_some()
    {
        return false;
    }
    let target_parent = world
        .get::<ChildOf>(target)
        .map(|child_of| child_of.parent());
    if target_parent.is_some_and(|parent| is_descendant(world, parent, dragged)) {
        return false;
    }
    let old_parent = world
        .get::<ChildOf>(dragged)
        .map(|child_of| child_of.parent());
    let mut siblings = siblings_of(world, target_parent);
    let before = siblings.clone();
    siblings.retain(|&entity| entity != dragged);
    let Some(index) = siblings.iter().position(|&entity| entity == target) else {
        return false;
    };
    siblings.insert(index + usize::from(after), dragged);
    if old_parent == target_parent && siblings == before {
        return false;
    }
    if old_parent != target_parent && !reparent(world, dragged, target_parent) {
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
pub fn duplicate_entity(world: &mut World, entity: Entity) -> Entity {
    let copy = clone_subtree(world, entity, None);
    if let Some(mut name) = world.get_mut::<Name>(copy) {
        name.0.push_str(" Copy");
    }
    copy
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

/// Recursively clones `src` under `parent`, returning the clone.
fn clone_subtree(world: &mut World, src: Entity, parent: Option<Entity>) -> Entity {
    let name = world.get::<Name>(src).map(|n| n.0.clone());
    let transform = world.get::<TransformComponent>(src).copied();
    let camera = world.get::<Camera>(src).copied();
    let asset = world.get::<AssetSource>(src).cloned();
    let disabled = is_disabled(world, src);
    let children = children_of(world, src);

    let mut clone = world.spawn_empty();
    if let Some(name) = name {
        clone.insert(Name::new(name));
    }
    if let Some(transform) = transform {
        clone.insert(transform);
    }
    if let Some(camera) = camera {
        clone.insert(camera);
    }
    if let Some(asset) = asset {
        clone.insert(asset);
    }
    if disabled {
        clone.insert(Disabled);
    }
    if let Some(parent) = parent {
        clone.insert(ChildOf(parent));
    }
    let new_entity = clone.id();

    for child in children {
        clone_subtree(world, child, Some(new_entity));
    }
    new_entity
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

/// Draws the hierarchy panel into `ui`: a toolbar (+ Cube / Duplicate /
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
        ui.label("Hierarchy");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            ui.menu_button("+", |ui| {
                crate::theme::style_context_menu(ui);
                if ui.button("Cube").clicked() {
                    let offset = roots(world).len() as f32 * 1.5;
                    let entity = spawn_entity(world, "Cube", Vec3::new(offset, 0.0, 0.0));
                    select_only(selected, secondary, entity);
                    ui.close();
                }
                let selection = selection_list(*selected, secondary);
                let has_selection = !selection.is_empty();
                if ui
                    .add_enabled(has_selection, egui::Button::new("Duplicate"))
                    .clicked()
                {
                    let mut last = None;
                    for source in &selection {
                        last = Some(duplicate_entity(world, *source));
                    }
                    if let Some(new) = last {
                        select_only(selected, secondary, new);
                    }
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
                .fill(crate::theme::SHELL_FILL)
                .stroke(egui::Stroke::new(1.0, crate::theme::BORDER_SOFT))
                .corner_radius(6)
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
            if let Some(dragged) = root_strip.dnd_release_payload::<Entity>()
                && reparent_at_end(world, *dragged, None)
            {
                select_only(selected, secondary, *dragged);
            } else if root_strip.dnd_hover_payload::<Entity>().is_some() {
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
                        end_drop_target(ui, world, last, selected, secondary);
                    }
                });
        });
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
        let drop_into =
            entity_drop_target(ui, world, entity, row.response.rect, selected, secondary);
        paint_hierarchy_row(ui, highlight, row.response.rect, in_selection || drop_into);
        return;
    }

    let id = ui.make_persistent_id(("hierarchy_row", entity));
    let mut collapsing =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
    if visible.is_some() {
        collapsing.set_open(true);
        collapsing.store(ui.ctx());
    }
    let header = ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add_space(4.0);
        crate::assets::tree_disclosure(ui, &mut collapsing);
        entity_row(ui, world, entity, selected, secondary, state);
    });
    let drop_into =
        entity_drop_target(ui, world, entity, header.response.rect, selected, secondary);
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
            end_drop_target(ui, world, last, selected, secondary);
        }
    });
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
) -> bool {
    let hit_rect = egui::Rect::from_min_max(rect.min - egui::vec2(0.0, 4.0), rect.max);
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let placement = if pointer.is_some_and(|pointer| pointer.y < rect.top()) {
        DropPlacement::Before
    } else {
        DropPlacement::Into
    };
    handle_entity_drop(ui, world, target, hit_rect, placement, selected, secondary)
}

fn end_drop_target(
    ui: &mut egui::Ui,
    world: &mut World,
    target: Entity,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
    handle_entity_drop(
        ui,
        world,
        target,
        rect,
        DropPlacement::After,
        selected,
        secondary,
    );
}

fn handle_entity_drop(
    ui: &mut egui::Ui,
    world: &mut World,
    target: Entity,
    hit_rect: egui::Rect,
    placement: DropPlacement,
    selected: &mut Option<Entity>,
    secondary: &mut Vec<Entity>,
) -> bool {
    let response = ui.interact(
        hit_rect,
        ui.make_persistent_id((
            "hierarchy_drop",
            target,
            matches!(placement, DropPlacement::After),
        )),
        egui::Sense::hover(),
    );
    let Some(dragged) = response.dnd_hover_payload::<Entity>() else {
        return false;
    };
    let dragged = *dragged;
    if dragged == target || world.get::<Lock>(dragged).is_some() {
        return false;
    }
    let parent = match placement {
        DropPlacement::Into => Some(target),
        DropPlacement::Before | DropPlacement::After => world
            .get::<ChildOf>(target)
            .map(|child_of| child_of.parent()),
    };
    if parent.is_some_and(|parent| is_descendant(world, parent, dragged)) {
        return false;
    }
    if response.dnd_release_payload::<Entity>().is_some() {
        let moved = match placement {
            DropPlacement::Before => reorder_relative(world, dragged, target, false),
            DropPlacement::After => reorder_relative(world, dragged, target, true),
            DropPlacement::Into => {
                let group = if *selected == Some(dragged) || secondary.contains(&dragged) {
                    selection_list(*selected, secondary)
                } else {
                    vec![dragged]
                };
                let mut moved = false;
                for child in group {
                    if world.get::<Lock>(child).is_none()
                        && !is_descendant(world, target, child)
                        && reparent_at_end(world, child, Some(target))
                    {
                        moved = true;
                    }
                }
                moved
            }
        };
        if moved {
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
                egui::Stroke::new(2.0, crate::theme::ACCENT),
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
        Some(crate::theme::ACCENT_FILL)
    } else if ui.rect_contains_pointer(rect) {
        Some(crate::theme::PANEL_HOVER)
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
    let stroke = egui::Stroke::new(1.3, crate::theme::MUTED);
    ui.painter().circle_stroke(center, 4.0, stroke);
    ui.painter().line_segment(
        [center + egui::vec2(3.0, 3.0), center + egui::vec2(6.0, 6.0)],
        stroke,
    );
}

fn paint_entity_icon(ui: &mut egui::Ui, camera: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.2, crate::theme::MUTED);
    let painter = ui.painter();
    if camera {
        painter.add(egui::Shape::line(
            vec![
                at(1.0, 3.0),
                at(9.0, 3.0),
                at(9.0, 11.0),
                at(1.0, 11.0),
                at(1.0, 3.0),
            ],
            stroke,
        ));
        painter.add(egui::Shape::line(
            vec![at(9.0, 5.0), at(13.0, 3.0), at(13.0, 11.0), at(9.0, 9.0)],
            stroke,
        ));
    } else {
        painter.add(egui::Shape::line(
            vec![
                at(7.0, 1.0),
                at(13.0, 4.0),
                at(13.0, 10.0),
                at(7.0, 13.0),
                at(1.0, 10.0),
                at(1.0, 4.0),
                at(7.0, 1.0),
            ],
            stroke,
        ));
        painter.line_segment([at(1.0, 4.0), at(7.0, 7.0)], stroke);
        painter.line_segment([at(13.0, 4.0), at(7.0, 7.0)], stroke);
        painter.line_segment([at(7.0, 7.0), at(7.0, 13.0)], stroke);
    }
}

fn paint_lock_icon(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.2, crate::theme::SUBTLE);
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
    let text = if disabled {
        egui::RichText::new(name)
            .color(crate::theme::SUBTLE)
            .italics()
    } else if *selected == Some(entity) || secondary.contains(&entity) {
        egui::RichText::new(name).color(crate::theme::TEXT)
    } else {
        egui::RichText::new(name).color(crate::theme::MUTED)
    };

    let row_id = egui::Id::new(("hierarchy_dnd", entity));
    let row_rect = ui
        .dnd_drag_source(row_id, entity, |ui| {
            let row_rect =
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 27.0));
            ui.horizontal(|ui| {
                paint_entity_icon(ui, world.get::<Camera>(entity).is_some());
                ui.allocate_ui_with_layout(
                    egui::vec2(
                        (ui.available_width() - if locked { 20.0 } else { 0.0 }).max(0.0),
                        27.0,
                    ),
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
    let response = ui.interact(row_rect, row_id, egui::Sense::click_and_drag());
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
    if response.double_clicked() {
        state.renaming = Some(Renaming {
            entity,
            draft: display_name(world, entity),
            focus: true,
        });
    }

    response.context_menu(|ui| {
        crate::theme::style_context_menu(ui);
        if ui.button("Rename").clicked() {
            state.renaming = Some(Renaming {
                entity,
                draft: display_name(world, entity),
                focus: true,
            });
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            select_only(selected, secondary, duplicate_entity(world, entity));
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
                if rect.fill == crate::theme::ACCENT_FILL && rect.rect.contains(target_label) =>
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
                        rect.fill == crate::theme::ACCENT_FILL && rect.rect.contains(to)
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
                if rect.fill == crate::theme::ACCENT_FILL
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
                        stroke.color == crate::theme::ACCENT
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
                if rect.fill == crate::theme::ACCENT_FILL
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

        assert!(reorder_relative(&mut world, dragged, target, false));
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

        assert!(!reorder_relative(&mut world, parent, descendant, false));
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
                rect.fill == crate::theme::ACCENT_FILL
                    && rect.rect.contains(arrow)
                    && rect.rect.contains(name.center())
            }
            _ => false,
        });
        output.textures_delta.clear();
        assert!(selected_fill, "selection must wrap the arrow and label");
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
        for (entity, label) in [(parent, "Parent"), (leaf, "Leaf")] {
            let mut selected = Some(entity);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(360.0, 280.0),
                    )),
                    ..Default::default()
                },
                |ui| show(ui, &mut world, &mut selected, &mut secondary, &mut state),
            );
            let name = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                }
                _ => None,
            });
            let name = name.unwrap();
            let selected_right = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == crate::theme::ACCENT_FILL && rect.rect.contains(name) =>
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
                rect.fill == crate::theme::PANEL_HOVER
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
        let duplicate = text_center(&menu, "Duplicate").unwrap();
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

        let copy = duplicate_entity(&mut world, parent);

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
}
