//! Shared per-frame editor state: everything panels read or write,
//! bundled into one type so [`crate::EditorShell::run_frame`] doesn't
//! grow an unbounded parameter list as more panels get added (it was
//! already at four before this — hierarchy/inspector's world + selected
//! entity, plus the asset browser's entries + selection — with console
//! and gizmos still to come).

use std::path::{Path, PathBuf};

use engine_ecs::components::{AssetSource, Disabled, Name, Transform as TransformComponent};
use engine_ecs::prelude::{Entity, Without, World};
use engine_project::Project;
use engine_scene::{Scene, SceneError, write_atomic};
use glam::Vec3;

use crate::assets::{AssetEntry, AssetIndex};
use crate::chrome::{
    BottomTab, BuildConfig, EditorDimension, InspectorTab, PanelVisibility, TransformSpace,
    Workspace,
};
use crate::console::{ConsoleFilter, ConsoleLog};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::dirty::{DirtyState, PendingAction};
use crate::error::EditorError;
use crate::gizmo::{Axis, GizmoMode};
use crate::hierarchy::{self, HierarchyState};
use crate::history::History;
use crate::import::AssetImporter;
use crate::inspector;
use crate::output::OutputLog;
use crate::play::PlayState;
use crate::preview::PreviewCache;
use crate::profiler::FrameProfiler;
use crate::session::{EditorSession, SESSION_VERSION};

/// Where the editor's "Save Scene" / "Open Scene" actions read and write,
/// relative to the editor's working directory.
pub const DEFAULT_SCENE_PATH: &str = "scene.ron";

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
    /// The entity the hierarchy panel most recently selected, if any —
    /// the "primary" of a multi-selection: the one the Inspector edits
    /// and the Scene-view gizmo follows.
    pub selected_entity: Option<Entity>,
    /// Entities selected *in addition to* [`EditorState::selected_entity`]
    /// (cmd/ctrl-click in the hierarchy). Never contains the primary and
    /// never has duplicates. Batch Delete / Duplicate act on the whole
    /// set; the gizmo and Inspector still target the primary only.
    pub secondary_selection: Vec<Entity>,
    /// Files found under the assets directory scanned at startup (see
    /// [`crate::scan_assets`]).
    pub assets: Vec<AssetEntry>,
    /// Stable-id ↔ current-path map for the scanned assets (see
    /// [`crate::resolve_asset_ids`]). Used to re-point an entity's
    /// [`AssetSource`] at a file that has been renamed or moved.
    pub asset_index: AssetIndex,
    /// Runs the per-kind importer for each referenceable asset, gated by
    /// an [`engine_asset::ImportCache`] so an unchanged source is not
    /// re-decoded. Filled at startup by [`AssetImporter::run`]; the
    /// decoded data is held in memory only (no GPU upload yet).
    pub importer: AssetImporter,
    /// The asset file the asset browser panel most recently selected, if
    /// any — a path relative to the scanned root.
    pub selected_asset: Option<PathBuf>,
    /// Cached preview render state (thumbnail / waveform) for
    /// `selected_asset`, rebuilt when the selection or import pass
    /// changes. See the `preview` module.
    pub preview: PreviewCache,
    /// Captured `tracing` events, shown in the console panel — see
    /// [`crate::ConsoleLayer`] for how it gets fed.
    pub console: ConsoleLog,
    /// The Console tab's level / search filter (display-only, persists
    /// across frames).
    pub console_filter: ConsoleFilter,
    /// Structured diagnostics shown in the bottom panel's Problems tab.
    /// Empty until the rust-analyzer / build bridges push into it.
    pub diagnostics: Diagnostics,
    /// Plain-text sink shown in the bottom panel's Output tab. Empty
    /// until the build pipeline / terminal write into it.
    pub output: OutputLog,
    /// The axis the translate gizmo is currently being dragged along, if
    /// a drag is in progress — `None` the rest of the time (including
    /// while an entity is selected but nothing's being dragged).
    pub dragging_axis: Option<Axis>,
    /// Where [`EditorState::save_scene`]/[`EditorState::load_scene`] read
    /// and write. Defaults to [`DEFAULT_SCENE_PATH`].
    pub scene_path: PathBuf,
    /// What a drag on a Scene View gizmo handle does — translate, rotate,
    /// or scale the selected entity.
    pub gizmo_mode: GizmoMode,
    /// Undo/redo stack of world snapshots — driven by the editor shell
    /// around each frame's UI pass (see [`History`]).
    pub history: History,
    /// Rolling CPU frame-time history, fed by the editor binary's loop
    /// and shown in the Profiler panel.
    pub profiler: FrameProfiler,
    /// Play-mode: run the scene (via the play schedule) and rewind on
    /// stop. See [`PlayState`].
    pub play: PlayState,
    /// The open project. Asset scanning, the scene path, and session
    /// save/restore all resolve against this.
    pub project: Project,
    /// The active studio workspace — a named panel layout the toolbar
    /// switches between.
    pub workspace: Workspace,
    /// Which panels are currently shown. Set from `workspace` on switch,
    /// then individually toggleable from the Window menu.
    pub panels: PanelVisibility,
    /// Remembered panel layout per workspace, indexed by
    /// [`Workspace::index`]. Switching workspaces stashes the current
    /// `panels` here and restores the target's — so each workspace keeps
    /// the tweaks the user made in it. Persisted via [`EditorSession`].
    pub workspace_layouts: [PanelVisibility; 4],
    /// Unsaved-change tracking across the scene, project settings, and
    /// open documents — drives the status bar, the window title, and the
    /// close guard.
    pub dirty: DirtyState,
    /// A guarded action (quit / new scene / revert) waiting on the
    /// "unsaved changes" dialog. `None` when no dialog is open.
    pub pending_action: Option<PendingAction>,
    /// The build profile shown in the toolbar (display-only for now).
    pub build_config: BuildConfig,
    /// The gizmo coordinate space shown in the toolbar (display-only for
    /// now — the gizmo still uses world axes).
    pub transform_space: TransformSpace,
    /// 3D vs 2D authoring: switches the viewport camera to an
    /// orthographic front view and constrains the gizmo to the screen
    /// plane. In-memory only (not persisted in the session yet).
    pub dimension: EditorDimension,
    /// When set (and more than one entity is selected), an Inspector
    /// field edit on the primary is copied to every other selected
    /// entity that has the same component.
    pub batch_edit: bool,
    /// Which tab the bottom Console / Problems / Output panel shows.
    pub bottom_tab: BottomTab,
    /// Which tab the right-side Inspector / Lighting panel shows.
    pub inspector_tab: InspectorTab,
    /// Set by the File → Exit menu item; the editor binary's
    /// `PlatformHandler::should_exit` reads it to leave the event loop.
    pub exit_requested: bool,
    /// A project-relative path the Asset Browser asked to open in the
    /// code editor (double-click on a text file). The editor binary
    /// drains this, resolves it against the assets dir, and forwards it
    /// to the Monaco webview.
    pub file_open_request: Option<PathBuf>,
    /// A diagnostic the user clicked in the Problems tab, for the editor
    /// binary to open at its source location. The `file` is
    /// project-root-relative and `line` is 1-based. Drained each frame.
    pub problem_jump: Option<Diagnostic>,
    /// Which Problems-tab row is highlighted (index into
    /// [`Diagnostics::snapshot`]). Shell-internal: the panel sets it on a
    /// click and immediately resolves it into `problem_jump`.
    pub(crate) problem_selected: Option<usize>,
    /// The Code panel's on-screen rectangle in logical points
    /// `[min_x, min_y, width, height]`, written every frame. The editor
    /// binary scales this to physical pixels to position the webview.
    pub code_panel_rect: Option<[f32; 4]>,
    /// Whether the code-editor webview should be shown this frame — false
    /// when the Code panel is hidden or a modal is open (egui can't draw
    /// over a native view).
    pub code_panel_visible: bool,
    /// The Code panel rect from last frame — the shell compares it to
    /// this frame's to detect an in-progress drag-resize.
    pub(crate) prev_code_panel_rect: Option<[f32; 4]>,
    /// Set by the shell while the Code panel is being drag-resized. The
    /// host hides the Monaco webview for that frame so the native view
    /// doesn't lag/overhang the panel edge mid-drag.
    pub code_panel_resizing: bool,
    /// Set by the shell when Ctrl/Cmd+S is pressed while the Code panel
    /// is shown; the host forwards it to Monaco (`CodeEditor::request_save`
    /// in `editor_code`). Lets save work even when egui (not the webview)
    /// holds keyboard focus.
    pub code_save_requested: bool,
    /// Caret line reported by the code editor (1-based), shown in the
    /// status bar.
    pub cursor_line: u32,
    /// Caret column reported by the code editor (1-based).
    pub cursor_column: u32,
    /// The integrated terminal, spawned lazily the first time its tab is
    /// viewed. Only present in builds with the `terminal` feature.
    #[cfg(feature = "terminal")]
    pub terminal: Option<crate::terminal::Terminal>,
    /// The hierarchy panel's search text and in-progress inline rename.
    pub hierarchy: HierarchyState,
    /// The Search tab's query, results, and pending jump.
    pub search: crate::search::SearchState,
    /// A component type name whose "Open Script" button was clicked in
    /// the Inspector; the host resolves it to a source location and
    /// opens it. Drained each frame.
    pub open_script_request: Option<String>,
    /// An entity the user asked to save as a `.prefab` (hierarchy "Make
    /// Prefab"). The host writes the file and refreshes the asset list.
    /// Drained each frame.
    pub create_prefab_request: Option<Entity>,
    /// Set at startup when unsaved scene edits were recovered from a
    /// previous session's autosave (see
    /// [`EditorState::recover_from_autosave`]). Purely informational —
    /// the world already holds the recovered state and the scene is
    /// marked dirty.
    pub recovered_from_autosave: bool,
}

/// File name of the per-project scene autosave, under `.studio/`.
pub const AUTOSAVE_FILE: &str = "autosave.ron";

/// Whether `autosave` represents unsaved work worth recovering: the file
/// exists and is strictly newer than `scene` (or `scene` doesn't exist).
/// A missing / unstattable `autosave` is never a candidate.
pub fn recovery_candidate(autosave: &Path, scene: &Path) -> bool {
    let Ok(autosave_meta) = std::fs::metadata(autosave) else {
        return false;
    };
    let Ok(autosave_time) = autosave_meta.modified() else {
        return false;
    };
    match std::fs::metadata(scene).and_then(|meta| meta.modified()) {
        Ok(scene_time) => autosave_time > scene_time,
        // No scene file to compare against — any autosave is a recovery.
        Err(_) => true,
    }
}

impl EditorState {
    /// Starts inside `project` from `world`, `assets`, and `console` with
    /// nothing selected and no drag in progress. The scene path defaults
    /// to the project's `main_scene`; [`EditorState::apply_session`] may
    /// override it.
    pub fn new(
        project: Project,
        world: World,
        assets: Vec<AssetEntry>,
        console: ConsoleLog,
    ) -> Self {
        let scene_path = project.main_scene_path();
        Self {
            world,
            selected_entity: None,
            secondary_selection: Vec::new(),
            assets,
            asset_index: AssetIndex::new(),
            importer: AssetImporter::new(),
            selected_asset: None,
            preview: PreviewCache::new(),
            console,
            console_filter: ConsoleFilter::default(),
            diagnostics: Diagnostics::new(),
            output: OutputLog::new(),
            dragging_axis: None,
            scene_path,
            gizmo_mode: GizmoMode::default(),
            history: History::new(),
            profiler: FrameProfiler::new(),
            play: PlayState::new(),
            project,
            workspace: Workspace::default(),
            panels: PanelVisibility::default(),
            workspace_layouts: Workspace::ALL.map(Workspace::visibility),
            dirty: DirtyState::default(),
            pending_action: None,
            build_config: BuildConfig::default(),
            transform_space: TransformSpace::default(),
            dimension: EditorDimension::default(),
            batch_edit: false,
            bottom_tab: BottomTab::default(),
            inspector_tab: InspectorTab::default(),
            exit_requested: false,
            file_open_request: None,
            problem_jump: None,
            problem_selected: None,
            code_panel_rect: None,
            code_panel_visible: false,
            prev_code_panel_rect: None,
            code_panel_resizing: false,
            code_save_requested: false,
            cursor_line: 1,
            cursor_column: 1,
            #[cfg(feature = "terminal")]
            terminal: None,
            hierarchy: HierarchyState::default(),
            search: crate::search::SearchState::default(),
            open_script_request: None,
            create_prefab_request: None,
            recovered_from_autosave: false,
        }
    }

    /// Flags the scene as having unsaved edits. Called after any settled
    /// undo step or scene-mutating menu action.
    pub fn mark_dirty(&mut self) {
        self.dirty.mark_scene();
    }

    /// Replaces the world with a fresh empty scene: clears the selection
    /// and any in-progress gizmo drag, resets the undo history, and marks
    /// the scene dirty (the empty world is not yet on disk). The scene
    /// path is unchanged, so the next save writes over the same file.
    pub fn new_scene(&mut self) {
        self.world = World::new();
        self.clear_selection();
        self.dragging_axis = None;
        self.history = History::new();
        self.dirty.mark_scene();
    }

    /// Switches the active workspace and applies its panel-visibility
    /// preset. Does not touch the world, the scene, or the undo history.
    pub fn set_workspace(&mut self, workspace: Workspace) {
        self.workspace_layouts[self.workspace.index()] = self.panels;
        self.workspace = workspace;
        self.panels = self.workspace_layouts[workspace.index()];
    }

    /// Resets every workspace's remembered layout to its built-in preset
    /// and re-applies the current one — the "Reset Layouts" action.
    pub fn reset_workspace_layouts(&mut self) {
        self.workspace_layouts = Workspace::ALL.map(Workspace::visibility);
        self.panels = self.workspace_layouts[self.workspace.index()];
    }

    /// Restores editor-only state from a loaded [`EditorSession`]: the
    /// workspace, the exact panel visibility (which may differ from the
    /// workspace preset), and — if its file still exists inside the
    /// project — the last-open scene path.
    pub fn apply_session(&mut self, session: EditorSession) {
        self.workspace = session.workspace;
        self.panels = session.panels;
        for (slot, saved) in self
            .workspace_layouts
            .iter_mut()
            .zip(session.layouts.iter())
        {
            *slot = *saved;
        }
        if let Some(relative) = session.last_scene
            && let Ok(absolute) = self.project.resolve(&relative)
            && absolute.is_file()
        {
            self.scene_path = absolute;
        }
    }

    /// Snapshots the current editor-only state into an [`EditorSession`]
    /// for persisting to the project's `.studio/` directory.
    pub fn capture_session(&self) -> EditorSession {
        let last_scene = self
            .scene_path
            .strip_prefix(self.project.root())
            .ok()
            .map(Path::to_path_buf);
        // Fold the current live layout into its workspace slot so a
        // switch-then-save keeps in-workspace tweaks.
        let mut layouts = self.workspace_layouts;
        layouts[self.workspace.index()] = self.panels;

        EditorSession {
            version: SESSION_VERSION,
            workspace: self.workspace,
            panels: self.panels,
            layouts: layouts.to_vec(),
            last_scene,
        }
    }

    /// Captures the current world to a [`Scene`] and writes it to
    /// [`EditorState::scene_path`] as RON.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError`] if serialization or the file write fails.
    pub fn save_scene(&mut self) -> Result<(), SceneError> {
        let scene = Scene::from_world(&mut self.world);
        scene.save_to_file(&self.scene_path)?;
        self.dirty.clear_scene();
        // The scene on disk now matches the world — drop any crash
        // autosave so it can't shadow a clean file next launch.
        self.clear_autosave();
        Ok(())
    }

    /// Path of this project's scene autosave (`.studio/autosave.ron`).
    pub fn autosave_path(&self) -> PathBuf {
        self.project.studio_dir().join(AUTOSAVE_FILE)
    }

    /// Writes the current world to the autosave file atomically, but
    /// only if the scene has unsaved edits. A no-op (returns `Ok`)
    /// otherwise. Does **not** clear the dirty flag — this is a crash
    /// backup, not a real save.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if serialization or the atomic write fails.
    pub fn autosave_scene(&mut self) -> Result<(), SceneError> {
        if !self.dirty.scene() {
            return Ok(());
        }
        let scene = Scene::from_world(&mut self.world);
        let text = scene.to_ron_string()?;
        write_atomic(&self.autosave_path(), text.as_bytes())
            .map_err(|err| SceneError::Io(err.to_string()))
    }

    /// Deletes the autosave file (best effort — a missing file is fine).
    pub fn clear_autosave(&mut self) {
        let path = self.autosave_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// If an autosave newer than the scene file is present (a previous
    /// session ended without saving), loads it into the world **instead
    /// of** the scene, marks the scene dirty, and sets
    /// [`EditorState::recovered_from_autosave`].
    ///
    /// Call once at startup, right after [`EditorState::load_scene`].
    /// Returns whether a recovery happened.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the autosave file exists but can't be parsed /
    /// validated (in which case the already-loaded scene is left in
    /// place and the stale autosave is removed).
    pub fn recover_from_autosave(&mut self) -> Result<bool, SceneError> {
        let autosave = self.autosave_path();
        if !recovery_candidate(&autosave, &self.scene_path) {
            return Ok(false);
        }
        match Scene::load_from_file(&autosave) {
            Ok(scene) => {
                let mut world = World::new();
                scene.instantiate(&mut world);
                self.world = world;
                self.clear_selection();
                self.dirty.mark_scene();
                self.heal_asset_sources();
                self.recovered_from_autosave = true;
                Ok(true)
            }
            Err(err) => {
                tracing::warn!(
                    path = %autosave.display(),
                    error = %err,
                    "autosave file is unreadable; discarding it"
                );
                self.clear_autosave();
                Err(err)
            }
        }
    }

    /// Spawns a new entity for `asset_path` (dragged in from the asset
    /// browser): a [`Name`] from the file stem, an identity `Transform`,
    /// and an [`AssetSource`] holding the path. Returns the new entity.
    ///
    /// Nothing is imported or rendered from the asset yet — the entity
    /// shows in the hierarchy and as a placeholder cube in the viewport,
    /// and the reference survives save/load.
    pub fn spawn_asset_entity(&mut self, asset_path: &Path) -> Entity {
        let name = asset_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Asset")
            .to_string();
        let source = match self.asset_index.id_for(asset_path) {
            Some(id) => {
                AssetSource::with_id(asset_path.to_string_lossy().into_owned(), id.to_string())
            }
            None => AssetSource::new(asset_path.to_string_lossy().into_owned()),
        };
        self.world
            .spawn((Name::new(name), TransformComponent::default(), source))
            .id()
    }

    /// Loads a `.prefab` file (`prefab_path` relative to the assets
    /// root) and instantiates it into the world, marking the scene
    /// dirty. The new entity is returned; the world change becomes an
    /// undo step via the shell's end-of-frame history settle.
    ///
    /// # Errors
    ///
    /// [`EditorError::Prefab`] if the file can't be read or parsed.
    pub fn spawn_prefab(&mut self, prefab_path: &Path) -> Result<Entity, EditorError> {
        let absolute = self.project.assets_dir().join(prefab_path);
        let prefab = engine_scene::Prefab::load_from_file(&absolute)?;
        let entity = prefab.instantiate(&mut self.world);
        self.dirty.mark_scene();
        Ok(entity)
    }

    /// Re-points every [`AssetSource`] in the world at the current path
    /// for its stable id (via [`EditorState::asset_index`]), and
    /// backfills a missing id from the path. Called right after loading a
    /// scene so a reference to a renamed/moved asset still points at the
    /// right file. Marks the scene dirty when it changes anything, so a
    /// save persists the healed reference.
    fn heal_asset_sources(&mut self) {
        let mut healed = 0u32;
        let mut query = self.world.query::<&mut AssetSource>();
        for mut source in query.iter_mut(&mut self.world) {
            match source.id.as_deref().and_then(crate::assets::parse_asset_id) {
                Some(id) => {
                    if let Some(current) = self.asset_index.path_for(id) {
                        let current = current.to_string_lossy().into_owned();
                        if source.path != current {
                            source.path = current;
                            healed += 1;
                        }
                    }
                }
                None => {
                    if let Some(id) = self.asset_index.id_for(Path::new(&source.path)) {
                        source.id = Some(id.to_string());
                        healed += 1;
                    }
                }
            }
        }
        if healed > 0 {
            tracing::info!(count = healed, "re-resolved asset references after load");
            self.dirty.mark_scene();
        }
    }

    /// Loads a [`Scene`] from [`EditorState::scene_path`], replacing the
    /// current world with its entities and clearing the selection.
    ///
    /// The world is only replaced once the file has parsed and validated,
    /// so a bad file leaves the current scene untouched.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError`] if the file can't be read, isn't valid RON,
    /// or fails [`Scene::validate`].
    pub fn load_scene(&mut self) -> Result<(), SceneError> {
        let scene = Scene::load_from_file(Path::new(&self.scene_path))?;
        scene.validate()?;

        let mut world = World::new();
        scene.instantiate(&mut world);
        self.world = world;
        self.clear_selection();
        self.dirty.clear_scene();
        self.heal_asset_sources();
        Ok(())
    }

    /// Every entity in the selection: the primary (if any) first, then
    /// [`EditorState::secondary_selection`].
    pub fn selection(&self) -> Vec<Entity> {
        hierarchy::selection_list(self.selected_entity, &self.secondary_selection)
    }

    /// How many entities are selected in total.
    pub fn selection_len(&self) -> usize {
        usize::from(self.selected_entity.is_some()) + self.secondary_selection.len()
    }

    /// Makes `entity` the only selected entity (primary, empty
    /// secondary).
    pub fn select_only(&mut self, entity: Entity) {
        hierarchy::select_only(
            &mut self.selected_entity,
            &mut self.secondary_selection,
            entity,
        );
    }

    /// Toggles `entity` in the selection — see
    /// [`crate::hierarchy::select_toggle`].
    pub fn select_toggle(&mut self, entity: Entity) {
        hierarchy::select_toggle(
            &mut self.selected_entity,
            &mut self.secondary_selection,
            entity,
        );
    }

    /// Clears the whole selection.
    pub fn clear_selection(&mut self) {
        self.selected_entity = None;
        self.secondary_selection.clear();
    }

    /// Despawns every selected entity (and its descendants) and clears
    /// the selection. The world change becomes one undo step via the
    /// shell's end-of-frame history settle.
    pub fn delete_selection(&mut self) {
        for entity in self.selection() {
            hierarchy::despawn_entity(&mut self.world, entity);
        }
        self.clear_selection();
    }

    /// Deep-copies every selected entity; the last copy becomes the new
    /// primary selection. No-op on an empty selection.
    pub fn duplicate_selection(&mut self) {
        let mut last = None;
        for source in self.selection() {
            last = Some(hierarchy::duplicate_entity(&mut self.world, source));
        }
        if let Some(new) = last {
            self.select_only(new);
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

    /// Turns a clicked Problems-tab row (`problem_selected`) into a
    /// `problem_jump` — a clone of that diagnostic, but only if it
    /// carries a source `file` — and clears the selection. Out-of-range
    /// or file-less rows just clear it. Called once per frame by the
    /// bottom panel.
    pub(crate) fn resolve_problem_jump(&mut self) {
        let Some(index) = self.problem_selected.take() else {
            return;
        };
        if let Some(diagnostic) = self.diagnostics.snapshot().into_iter().nth(index)
            && diagnostic.file.is_some()
        {
            self.problem_jump = Some(diagnostic);
        }
    }

    /// Every entity's current `Transform`, in no particular (but stable
    /// within a frame) order — what [`crate::Viewport::render`] draws
    /// its placeholder cubes at, one per entry.
    pub fn entity_transforms(&mut self) -> Vec<engine_utils::Transform> {
        let mut query = self
            .world
            .query_filtered::<&TransformComponent, Without<Disabled>>();
        query
            .iter(&self.world)
            .map(|transform| transform.0)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Workspace;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A temp project that cleans its directory up on drop.
    struct TempProject(Project);

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.root());
        }
    }

    fn temp_project() -> TempProject {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("vge_editor_state_{nanos}_{n}"));
        TempProject(Project::create(&root, "Test").expect("create temp project"))
    }

    fn editor_state(project: &TempProject) -> EditorState {
        EditorState::new(
            project.0.clone(),
            World::new(),
            Vec::new(),
            ConsoleLog::new(),
        )
    }

    #[test]
    fn new_starts_clean_in_the_world_workspace_with_the_project_scene() {
        let project = temp_project();
        let state = editor_state(&project);
        assert!(!state.dirty.any());
        assert!(state.pending_action.is_none());
        assert!(!state.exit_requested);
        assert_eq!(state.workspace, Workspace::World);
        assert_eq!(state.panels, Workspace::World.visibility());
        assert_eq!(state.scene_path, project.0.main_scene_path());
    }

    #[test]
    fn mark_dirty_sets_the_scene_flag() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.mark_dirty();
        assert!(state.dirty.scene() && state.dirty.any());
    }

    #[test]
    fn heal_asset_sources_repoints_by_id_and_backfills_from_path() {
        use engine_asset::AssetId;
        use engine_ecs::components::AssetSource;

        let project = temp_project();
        let mut state = editor_state(&project);

        let known = AssetId::new();
        let known_text = known.to_string();
        state
            .asset_index
            .insert(known, PathBuf::from("moved/barrel.gltf"));

        // Stale path + known id → path is re-pointed to the current one.
        state.world.spawn((
            Name::new("Barrel"),
            AssetSource::with_id("old/barrel.gltf", known_text.clone()),
        ));
        // No id, but the path is in the index → id is backfilled.
        state
            .world
            .spawn((Name::new("Backfill"), AssetSource::new("moved/barrel.gltf")));
        // Id the index doesn't know → left untouched.
        let stray_text = AssetId::new().to_string();
        state.world.spawn((
            Name::new("Stray"),
            AssetSource::with_id("weird/thing.gltf", stray_text.clone()),
        ));

        state.dirty.clear_scene();
        state.heal_asset_sources();

        let mut query = state.world.query::<(&Name, &AssetSource)>();
        let by_name: std::collections::HashMap<String, AssetSource> = query
            .iter(&state.world)
            .map(|(name, source)| (name.0.clone(), source.clone()))
            .collect();

        assert_eq!(by_name["Barrel"].path, "moved/barrel.gltf");
        assert_eq!(by_name["Barrel"].id.as_deref(), Some(known_text.as_str()));
        assert_eq!(by_name["Backfill"].id.as_deref(), Some(known_text.as_str()));
        assert_eq!(by_name["Stray"].path, "weird/thing.gltf");
        assert_eq!(by_name["Stray"].id.as_deref(), Some(stray_text.as_str()));
        assert!(
            state.dirty.scene(),
            "a healed reference marks the scene dirty"
        );
    }

    #[test]
    fn resolve_problem_jump_only_fires_for_rows_with_a_file() {
        use crate::diagnostics::{Diagnostic, Severity};

        let project = temp_project();
        let mut state = editor_state(&project);
        state.diagnostics.set(vec![
            Diagnostic {
                severity: Severity::Warning,
                code: None,
                message: "no location".to_string(),
                file: None,
                line: None,
            },
            Diagnostic {
                severity: Severity::Error,
                code: Some("E0599".to_string()),
                message: "no method".to_string(),
                file: Some(PathBuf::from("src/player.rs")),
                line: Some(7),
            },
        ]);

        // Row 0 has no file: selection clears, no jump.
        state.problem_selected = Some(0);
        state.resolve_problem_jump();
        assert!(state.problem_jump.is_none());
        assert!(state.problem_selected.is_none());

        // Row 1 has a file: jump carries that diagnostic.
        state.problem_selected = Some(1);
        state.resolve_problem_jump();
        let jump = state
            .problem_jump
            .take()
            .expect("a jump for the located row");
        assert_eq!(jump.file, Some(PathBuf::from("src/player.rs")));
        assert_eq!(jump.line, Some(7));

        // Stale / out-of-range index: ignored.
        state.problem_selected = Some(99);
        state.resolve_problem_jump();
        assert!(state.problem_jump.is_none());
        assert!(state.problem_selected.is_none());
    }

    #[test]
    fn new_scene_empties_the_world_and_marks_dirty() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let entity = state.world.spawn(Name::new("Gone")).id();
        state.selected_entity = Some(entity);
        state.mark_dirty();
        state.save_scene().expect("save");
        assert!(!state.dirty.any());

        state.new_scene();

        let named = state.world.query::<&Name>().iter(&state.world).count();
        assert_eq!(named, 0, "no scene entities remain");
        assert!(state.selected_entity.is_none(), "selection cleared");
        assert!(!state.history.can_undo(), "history reset");
        assert!(state.dirty.scene(), "empty scene is unsaved");
    }

    #[test]
    fn save_scene_clears_only_the_scene_not_open_documents() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.mark_dirty();
        state.dirty.mark_document("src/player.rs");

        state.save_scene().expect("save");

        assert!(!state.dirty.scene(), "scene saved");
        assert!(state.dirty.any(), "the dirty document still counts");
    }

    #[test]
    fn set_workspace_swaps_panels_without_touching_the_world() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let entity = state.world.spawn(Name::new("Keep")).id();
        state.selected_entity = Some(entity);

        state.set_workspace(Workspace::Code);

        assert_eq!(state.workspace, Workspace::Code);
        assert_eq!(state.panels, Workspace::Code.visibility());
        assert!(state.world.get::<Name>(entity).is_some(), "entity survives");
        assert_eq!(state.selected_entity, Some(entity), "selection survives");
    }

    #[test]
    fn save_scene_clears_the_dirty_flag() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.mark_dirty();

        state.save_scene().expect("save to the project scene path");
        assert!(!state.dirty.any());
    }

    #[test]
    fn load_scene_clears_the_dirty_flag() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.save_scene().expect("write a scene to load back");
        state.mark_dirty();

        state.load_scene().expect("load the project scene");
        assert!(!state.dirty.any());
    }

    #[test]
    fn capture_then_apply_session_round_trips_workspace_and_panels() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.set_workspace(Workspace::Debug);
        state.panels.ai = true; // diverge from the Debug preset

        let captured = state.capture_session();

        let mut restored = editor_state(&project);
        restored.apply_session(captured);
        assert_eq!(restored.workspace, Workspace::Debug);
        assert_eq!(restored.panels, state.panels);
    }

    #[test]
    fn apply_session_adopts_an_existing_last_scene() {
        let project = temp_project();
        let mut state = editor_state(&project);

        let session = EditorSession {
            version: SESSION_VERSION,
            workspace: Workspace::World,
            panels: PanelVisibility::default(),
            layouts: Vec::new(),
            last_scene: Some(PathBuf::from("scenes/main.ron")),
        };
        state.apply_session(session);

        assert_eq!(state.scene_path, project.0.main_scene_path());
    }

    #[test]
    fn apply_session_ignores_a_missing_last_scene() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let before = state.scene_path.clone();

        let session = EditorSession {
            version: SESSION_VERSION,
            workspace: Workspace::World,
            panels: PanelVisibility::default(),
            layouts: Vec::new(),
            last_scene: Some(PathBuf::from("scenes/does_not_exist.ron")),
        };
        state.apply_session(session);

        assert_eq!(
            state.scene_path, before,
            "unchanged when the file is absent"
        );
    }

    fn spawn_named(state: &mut EditorState, name: &str) -> Entity {
        state.world.spawn(Name::new(name)).id()
    }

    #[test]
    fn select_only_and_toggle_maintain_the_selection_set() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let a = spawn_named(&mut state, "A");
        let b = spawn_named(&mut state, "B");
        let c = spawn_named(&mut state, "C");

        state.select_only(a);
        assert_eq!(state.selection(), vec![a]);
        assert_eq!(state.selection_len(), 1);

        state.select_toggle(b);
        state.select_toggle(c);
        assert_eq!(state.selection(), vec![a, b, c]);
        assert_eq!(state.selection_len(), 3);

        state.select_toggle(b); // drop a secondary
        assert_eq!(state.selection(), vec![a, c]);

        state.select_only(c); // collapses back to one
        assert_eq!(state.selection(), vec![c]);
        assert!(state.secondary_selection.is_empty());
    }

    #[test]
    fn delete_selection_despawns_every_selected_entity() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let a = spawn_named(&mut state, "A");
        let b = spawn_named(&mut state, "B");
        let keep = spawn_named(&mut state, "Keep");

        state.select_only(a);
        state.select_toggle(b);
        state.delete_selection();

        assert_eq!(state.selection_len(), 0);
        assert!(state.world.get_entity(a).is_err());
        assert!(state.world.get_entity(b).is_err());
        assert!(state.world.get_entity(keep).is_ok());
    }

    #[test]
    fn duplicate_selection_copies_each_and_reprimaries() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let a = spawn_named(&mut state, "A");
        let b = spawn_named(&mut state, "B");

        state.select_only(a);
        state.select_toggle(b);
        state.duplicate_selection();

        // Two originals + two copies.
        let mut query = state.world.query::<&Name>();
        assert_eq!(query.iter(&state.world).count(), 4);
        // Selection collapsed to the last copy.
        assert_eq!(state.selection_len(), 1);
        assert!(state.secondary_selection.is_empty());
    }

    #[test]
    fn recovery_candidate_only_fires_for_a_newer_autosave() {
        let dir = std::env::temp_dir().join(format!(
            "vge-recovery-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let scene = dir.join("main.ron");
        let autosave = dir.join("autosave.ron");

        assert!(!recovery_candidate(&autosave, &scene), "neither exists");

        std::fs::write(&scene, b"()").unwrap();
        assert!(!recovery_candidate(&autosave, &scene), "no autosave");

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&autosave, b"()").unwrap();
        assert!(recovery_candidate(&autosave, &scene), "autosave is newer");

        // A scene missing entirely still counts as recoverable.
        std::fs::remove_file(&scene).unwrap();
        assert!(recovery_candidate(&autosave, &scene));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn autosave_writes_only_when_the_scene_is_dirty_then_save_clears_it() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.scene_path = project.0.main_scene_path();

        // Clean scene → no autosave file.
        state.autosave_scene().unwrap();
        assert!(!state.autosave_path().exists());

        // Dirty → autosave appears.
        state.world.spawn(Name::new("Cube"));
        state.dirty.mark_scene();
        state.autosave_scene().unwrap();
        assert!(state.autosave_path().exists());

        // An explicit save clears it.
        std::fs::create_dir_all(state.scene_path.parent().unwrap()).ok();
        state.save_scene().unwrap();
        assert!(!state.autosave_path().exists());
    }

    #[test]
    fn set_workspace_remembers_each_workspace_layout() {
        let project = temp_project();
        let mut state = editor_state(&project);

        // Hide the console in the World workspace.
        state.panels.console = false;
        state.set_workspace(Workspace::Code);
        // Code has its own preset (console on).
        assert!(state.panels.console);
        // Tweak Code, go back to World.
        state.panels.hierarchy = false;
        state.set_workspace(Workspace::World);
        assert!(!state.panels.console, "World kept its console-off tweak");
        state.set_workspace(Workspace::Code);
        assert!(!state.panels.hierarchy, "Code kept its hierarchy-off tweak");

        // Round-trips through the session.
        let session = state.capture_session();
        assert_eq!(session.layouts.len(), Workspace::ALL.len());
        let mut fresh = editor_state(&project);
        fresh.apply_session(session);
        fresh.set_workspace(Workspace::World);
        assert!(!fresh.panels.console);
    }
}
