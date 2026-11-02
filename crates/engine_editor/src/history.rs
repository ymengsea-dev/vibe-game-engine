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
//!
//! Terrain sculpting changes files on disk, not the world, so a stroke is
//! recorded explicitly with [`History::record_terrain`]. It shares the same
//! stack, so undo always reverses the most recent edit of either kind.

use std::path::PathBuf;

use engine_ecs::components::Name;
use engine_ecs::prelude::{Entity, World};
use engine_scene::Scene;

use crate::terrain_tools::{TerrainEdit, write_terrain};

/// Most undo steps kept — the oldest is dropped past this.
const DEFAULT_LIMIT: usize = 128;
/// Memory budget for terrain steps on the undo stack; the oldest terrain
/// steps are dropped past this so long sculpting sessions stay bounded.
const TERRAIN_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// What an undo or redo changed, so the caller can refresh dependent views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryChange {
    /// The world was replaced; entity ids changed.
    World,
    /// A terrain's files were rewritten; its mesh must be reloaded.
    Terrain {
        /// Absolute path of the terrain's `.gltf` model.
        model_path: PathBuf,
    },
}

/// An undo or redo step that could not be applied. The step stays on its
/// stack, so the user can retry once the cause is fixed.
#[derive(Debug, thiserror::Error)]
#[error("could not restore terrain {}: {source}", path.display())]
pub struct HistoryError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

enum Step {
    World(Snapshot),
    Terrain(TerrainEdit),
}

impl Step {
    fn terrain_bytes(&self) -> usize {
        match self {
            Self::World(_) => 0,
            Self::Terrain(edit) => edit.byte_size(),
        }
    }
}

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

/// An undo/redo stack of world snapshots and terrain strokes.
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
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
            self.push_new(Step::World(pending));
            true
        } else {
            false
        }
    }

    /// Records a finished terrain stroke as one undo step.
    pub fn record_terrain(&mut self, edit: TerrainEdit) {
        self.push_new(Step::Terrain(edit));
    }

    /// Pushes a fresh edit: clears redo and enforces the step and memory limits.
    fn push_new(&mut self, step: Step) {
        self.undo.push(step);
        self.redo.clear();
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        let mut terrain_bytes: usize = self.undo.iter().map(Step::terrain_bytes).sum();
        while terrain_bytes > TERRAIN_BYTE_BUDGET {
            let Some(oldest) = self
                .undo
                .iter()
                .position(|step| matches!(step, Step::Terrain(_)))
            else {
                break;
            };
            terrain_bytes -= self.undo.remove(oldest).terrain_bytes();
        }
    }

    /// Reverts the most recent edit, moving it onto the redo stack.
    /// Returns what changed, or `None` when there was nothing to undo.
    pub fn undo(
        &mut self,
        world: &mut World,
        selected: &mut Option<Entity>,
    ) -> Result<Option<HistoryChange>, HistoryError> {
        self.replay(Direction::Undo, world, selected)
    }

    /// Re-applies the most recently undone edit, moving it back onto the
    /// undo stack. Returns what changed, or `None` when there was nothing to redo.
    pub fn redo(
        &mut self,
        world: &mut World,
        selected: &mut Option<Entity>,
    ) -> Result<Option<HistoryChange>, HistoryError> {
        self.replay(Direction::Redo, world, selected)
    }

    fn replay(
        &mut self,
        direction: Direction,
        world: &mut World,
        selected: &mut Option<Entity>,
    ) -> Result<Option<HistoryChange>, HistoryError> {
        let (from, to) = match direction {
            Direction::Undo => (&mut self.undo, &mut self.redo),
            Direction::Redo => (&mut self.redo, &mut self.undo),
        };
        let Some(step) = from.pop() else {
            return Ok(None);
        };
        match step {
            Step::World(snapshot) => {
                to.push(Step::World(Snapshot::capture(world, *selected)));
                snapshot.restore(world, selected);
                self.pending = None;
                Ok(Some(HistoryChange::World))
            }
            Step::Terrain(edit) => {
                let target = match direction {
                    Direction::Undo => &edit.before,
                    Direction::Redo => &edit.after,
                };
                match write_terrain(&edit.model_path, target) {
                    Ok(()) => {
                        let change = HistoryChange::Terrain {
                            model_path: edit.model_path.clone(),
                        };
                        to.push(Step::Terrain(edit));
                        Ok(Some(change))
                    }
                    Err(source) => {
                        let path = edit.model_path.clone();
                        from.push(Step::Terrain(edit));
                        Err(HistoryError { path, source })
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Undo,
    Redo,
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
        assert!(
            history
                .undo(&mut world, &mut selected)
                .expect("history")
                .is_some()
        );
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
        history.undo(&mut world, &mut selected).expect("history");

        assert!(history.can_redo());
        assert!(
            history
                .redo(&mut world, &mut selected)
                .expect("history")
                .is_some()
        );
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
        history.undo(&mut world, &mut selected).expect("history");
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

        history.undo(&mut world, &mut selected).expect("history");
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
        assert!(
            history
                .undo(&mut world, &mut selected)
                .expect("history")
                .is_none()
        );
    }

    fn terrain_fixture(tag: &str) -> (PathBuf, PathBuf, TerrainEdit) {
        use crate::terrain_tools::{TerrainToolState, sculpt_terrain, write_new_terrain};
        let root = std::env::temp_dir().join(format!("vge-history-{tag}-{}", uuid::Uuid::new_v4()));
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let model = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();
        let edit = sculpt_terrain(&model, &TerrainToolState::default(), &[glam::Vec2::ZERO])
            .unwrap()
            .expect("stroke changes the terrain");
        (root, model, edit)
    }

    fn saved_terrain(model: &std::path::Path) -> crate::terrain_tools::TerrainAsset {
        crate::terrain_tools::TerrainAsset::load(&model.with_extension("terrain")).unwrap()
    }

    #[test]
    fn terrain_strokes_and_world_edits_undo_in_reverse_order() {
        let (root, model, edit) = terrain_fixture("order");
        let (before, after) = (edit.before.clone(), edit.after.clone());
        let mut history = History::new();
        let mut world = world_with("Cube", 0.0);
        let mut selected = None;

        history.record_terrain(edit);
        history.begin_frame(&mut world, selected);
        {
            let mut query = world.query::<&mut Transform>();
            query.iter_mut(&mut world).next().unwrap().0.translation.x = 5.0;
        }
        assert!(history.settle(&mut world, selected));

        assert_eq!(
            history.undo(&mut world, &mut selected).unwrap(),
            Some(HistoryChange::World)
        );
        assert_eq!(only_x(&mut world), 0.0);
        assert_eq!(
            saved_terrain(&model),
            after,
            "terrain untouched by the world undo"
        );
        assert_eq!(
            history.undo(&mut world, &mut selected).unwrap(),
            Some(HistoryChange::Terrain {
                model_path: model.clone()
            })
        );
        assert_eq!(saved_terrain(&model), before);
        assert_eq!(
            history.redo(&mut world, &mut selected).unwrap(),
            Some(HistoryChange::Terrain {
                model_path: model.clone()
            })
        );
        assert_eq!(saved_terrain(&model), after);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_terrain_restore_keeps_the_step_for_retry() {
        let (root, model, edit) = terrain_fixture("failure");
        let mut history = History::new();
        let mut world = World::new();
        let mut selected = None;
        history.record_terrain(edit);
        // A directory where the source file belongs makes the restore fail.
        let source = model.with_extension("terrain");
        std::fs::remove_file(&source).unwrap();
        std::fs::create_dir(&source).unwrap();

        assert!(history.undo(&mut world, &mut selected).is_err());
        assert!(history.can_undo(), "step is kept");
        assert!(!history.can_redo());
        std::fs::remove_dir(&source).unwrap();
        assert!(history.undo(&mut world, &mut selected).unwrap().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn terrain_steps_are_dropped_oldest_first_past_the_memory_budget() {
        let (root, _model, edit) = terrain_fixture("budget");
        let mut history = History::new();
        let per_step = edit.byte_size();
        let fits = TERRAIN_BYTE_BUDGET / per_step;
        for _ in 0..fits + 3 {
            history.record_terrain(edit.clone());
        }
        let kept: usize = history.undo.iter().map(Step::terrain_bytes).sum();
        assert!(kept <= TERRAIN_BYTE_BUDGET);
        assert_eq!(history.undo.len(), fits.min(DEFAULT_LIMIT));
        let _ = std::fs::remove_dir_all(root);
    }
}
