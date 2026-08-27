//! Shared per-frame editor state: everything panels read or write,
//! bundled into one type so [`crate::EditorShell::run_frame`] doesn't
//! grow an unbounded parameter list as more panels get added (it was
//! already at four before this — hierarchy/inspector's world + selected
//! entity, plus the asset browser's entries + selection — with console
//! and gizmos still to come).

use std::path::PathBuf;

use engine_ecs::components::Transform as TransformComponent;
use engine_ecs::prelude::{Entity, World};
use glam::Vec3;

use crate::assets::AssetEntry;
use crate::console::ConsoleLog;
use crate::gizmo::Axis;
use crate::inspector;

/// Owned by the editor binary (`editor/src/main.rs`), passed to
/// [`crate::EditorShell::run_frame`] by mutable reference each frame —
/// mirrors how [`crate::Viewport`] is borrowed rather than owned by the
/// shell itself.
pub struct EditorState {
    /// The scene the hierarchy/inspector panels show — also what
    /// [`EditorState::entity_transforms`] draws from for
    /// [`crate::Viewport::render`], so an entity added here shows up in
    /// both places.
    pub world: World,
    /// The entity the hierarchy panel most recently selected, if any.
    pub selected_entity: Option<Entity>,
    /// Files found under the assets directory scanned at startup (see
    /// [`crate::scan_assets`]).
    pub assets: Vec<AssetEntry>,
    /// The asset file the asset browser panel most recently selected, if
    /// any — a path relative to the scanned root.
    pub selected_asset: Option<PathBuf>,
    /// Captured `tracing` events, shown in the console panel — see
    /// [`crate::ConsoleLayer`] for how it gets fed.
    pub console: ConsoleLog,
    /// The axis the translate gizmo is currently being dragged along, if
    /// a drag is in progress — `None` the rest of the time (including
    /// while an entity is selected but nothing's being dragged).
    pub dragging_axis: Option<Axis>,
}

impl EditorState {
    /// Starts from `world`, `assets`, and `console` with nothing
    /// selected and no drag in progress.
    pub fn new(world: World, assets: Vec<AssetEntry>, console: ConsoleLog) -> Self {
        Self {
            world,
            selected_entity: None,
            assets,
            selected_asset: None,
            console,
            dragging_axis: None,
        }
    }

    /// The selected entity's current translation, if it's selected and
    /// has a `Transform` — where the Scene View panel's translate gizmo
    /// (and [`crate::Viewport::render`]'s `gizmo_origin` argument) should
    /// draw. `None` hides the gizmo entirely.
    pub fn gizmo_origin(&self) -> Option<Vec3> {
        self.selected_entity
            .and_then(|entity| inspector::transform_of(&self.world, entity))
            .map(|transform| transform.translation)
    }

    /// Every entity's current `Transform`, in no particular (but stable
    /// within a frame) order — what [`crate::Viewport::render`] draws
    /// its placeholder cubes at, one per entry.
    pub fn entity_transforms(&mut self) -> Vec<engine_utils::Transform> {
        let mut query = self.world.query::<&TransformComponent>();
        query
            .iter(&self.world)
            .map(|transform| transform.0)
            .collect()
    }
}
