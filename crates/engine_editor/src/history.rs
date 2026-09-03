//! Undo/redo for editor world edits, by whole-world snapshot.
//!
//! Rather than a per-edit command log (which has to track entity-id
//! stability across spawn/despawn), each undo step is a full [`Scene`]
//! snapshot of the world. Editor scenes are small, so capturing and
//! restoring one is cheap, and it sidesteps id bookkeeping entirely — the
//! trade-off is coarser granularity (one step = one settled edit, not one
//! field change) and that entity ids change on restore, so the selection
//! is re-resolved by name.
//!
//! [`History::begin_frame`] snapshots the pre-edit state once per edit
//! sequence; [`History::settle`] commits it to the undo stack when the
//! user stops interacting and the world actually changed. The editor
//! shell drives both around its per-frame UI pass, so no panel needs to
//! know about undo at all.

use engine_ecs::components::Name;
use engine_ecs::prelude::{Entity, World};
use engine_scene::Scene;

/// Most undo steps kept — the oldest is dropped past this.
const DEFAULT_LIMIT: usize = 128;

/// One restorable editor state: the world as a [`Scene`], plus the name
/// of whatever was selected (ids don't survive a restore).
#[derive(Clone)]
struct Snapshot {
    scene: Scene,
    selected_name: Option<String>,
}

impl Snapshot {
    fn capture(world: &mut World, selected: Option<Entity>) -> Self {
        let selected_name =
            selected.and_then(|entity| world.get::<Name>(entity).map(|name| name.0.clone()));
        Self {
            scene: Scene::from_world(world),
            selected_name,
        }
    }

    fn restore(&self, world: &mut World, selected: &mut Option<Entity>) {
        let mut fresh = World::new();
        self.scene.instantiate(&mut fresh);
        *world = fresh;
        *selected = self.selected_name.as_deref().and_then(|name| {
            let mut query = world.query::<(Entity, &Name)>();
            query
                .iter(world)
                .find(|(_, entity_name)| entity_name.0 == name)
                .map(|(entity, _)| entity)
        });
    }
}

/// An undo/redo stack of world snapshots.
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Pre-edit state for the sequence currently in progress — taken at
    /// the first frame after the last settle, committed to `undo` by
    /// [`History::settle`] once the world changes and interaction stops.
    pending: Option<Snapshot>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            limit: DEFAULT_LIMIT,
        }
    }

    /// Whether [`History::undo`] would do anything.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`History::redo`] would do anything.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Ensures a pre-edit snapshot exists for the current edit sequence.
    /// Call once at the top of a frame, before running the editor UI.
    pub fn begin_frame(&mut self, world: &mut World, selected: Option<Entity>) {
        if self.pending.is_none() {
            self.pending = Some(Snapshot::capture(world, selected));
        }
    }

    /// If the world changed since [`History::begin_frame`]'s snapshot,
    /// pushes that snapshot onto the undo stack (clearing redo) and
    /// starts a fresh sequence. Call at the end of a frame **only when the
    /// user isn't mid-interaction** (no active pointer drag / text entry),
    /// so a multi-frame drag becomes one undo step.
    ///
    /// Returns whether a snapshot was pushed — i.e. whether the world
    /// actually changed this sequence. Callers use this to flag unsaved
    /// changes.
    pub fn settle(&mut self, world: &mut World, selected: Option<Entity>) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let current = Snapshot::capture(world, selected);
        if current.scene != pending.scene {
            self.undo.push(pending);
            if self.undo.len() > self.limit {
                self.undo.remove(0);
            }
            self.redo.clear();
            true
        } else {
            false
        }
    }

    /// Reverts to the previous undo snapshot, moving the current state
    /// onto the redo stack. Returns whether anything was undone.
    pub fn undo(&mut self, world: &mut World, selected: &mut Option<Entity>) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snapshot::capture(world, *selected));
        snapshot.restore(world, selected);
        self.pending = None;
        true
    }

    /// Re-applies the most recently undone snapshot, moving the current
    /// state back onto the undo stack. Returns whether anything was
    /// redone.
    pub fn redo(&mut self, world: &mut World, selected: &mut Option<Entity>) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snapshot::capture(world, *selected));
        snapshot.restore(world, selected);
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_ecs::components::Transform;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    fn world_with(name: &str, x: f32) -> World {
        let mut world = World::new();
        world.spawn((
            Name::new(name),
            Transform::from(MathTransform::from_translation(Vec3::new(x, 0.0, 0.0))),
        ));
        world
    }

    fn only_x(world: &mut World) -> f32 {
        let mut query = world.query::<&Transform>();
        query.iter(world).next().unwrap().0.translation.x
    }

    #[test]
    fn undo_restores_the_pre_edit_state() {
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);
        let mut selected = None;

        history.begin_frame(&mut world, selected);
        // Edit: move the cube.
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 5.0;
        }
        history.settle(&mut world, selected);

        assert!(history.can_undo());
        assert!(history.undo(&mut world, &mut selected));
        assert_eq!(only_x(&mut world), 0.0);
    }

    #[test]
    fn redo_reapplies_an_undone_edit() {
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);
        let mut selected = None;

        history.begin_frame(&mut world, selected);
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 5.0;
        }
        history.settle(&mut world, selected);
        history.undo(&mut world, &mut selected);

        assert!(history.can_redo());
        assert!(history.redo(&mut world, &mut selected));
        assert_eq!(only_x(&mut world), 5.0);
    }

    #[test]
    fn settle_without_a_change_records_nothing() {
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);

        history.begin_frame(&mut world, None);
        history.settle(&mut world, None);

        assert!(!history.can_undo());
    }

    #[test]
    fn a_new_edit_clears_the_redo_stack() {
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);
        let mut selected = None;

        history.begin_frame(&mut world, selected);
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 5.0;
        }
        history.settle(&mut world, selected);
        history.undo(&mut world, &mut selected);
        assert!(history.can_redo());

        // A fresh edit after an undo drops the redo history.
        history.begin_frame(&mut world, selected);
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 9.0;
        }
        history.settle(&mut world, selected);
        assert!(!history.can_redo());
    }

    #[test]
    fn undo_reselects_the_previously_selected_entity_by_name() {
        let mut history = History::new();
        let mut world = World::new();
        let cube = world.spawn(Name::new("Cube")).id();
        let mut selected = Some(cube);

        history.begin_frame(&mut world, selected);
        world.spawn(Name::new("Extra")); // a change, so settle records
        history.settle(&mut world, selected);

        history.undo(&mut world, &mut selected);
        // Selection survives as a (new-id) entity still named "Cube".
        let name = selected
            .and_then(|entity| world.get::<Name>(entity))
            .map(|n| n.0.clone());
        assert_eq!(name.as_deref(), Some("Cube"));
    }

    #[test]
    fn undo_on_empty_history_is_a_no_op() {
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);
        let mut selected = None;
        assert!(!history.undo(&mut world, &mut selected));
    }
}
