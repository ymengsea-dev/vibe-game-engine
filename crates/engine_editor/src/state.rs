//! Shared per-frame editor state: everything panels read or write,
#![cfg_attr(feature = "dock-shell", allow(dead_code))]
//! bundled into one type so [`crate::EditorShell::run_frame`] doesn't
//! grow an unbounded parameter list as more panels get added (it was
//! already at four before this — hierarchy/inspector's world + selected
//! entity, plus the asset browser's entries + selection — with console
//! and gizmos still to come).

use std::path::{Path, PathBuf};

use engine_ecs::components::{
    AssetSource, Disabled, MeshSource, Name, Sprite, SpriteSource, Transform as TransformComponent,
};
use engine_ecs::prelude::{ChildOf, Children, Entity, Without, World};
use engine_project::Project;
use engine_scene::{Scene, SceneError, write_atomic};
use glam::{Vec2, Vec3};

use crate::assets::{AssetBrowser, AssetEntry, AssetIndex};
use crate::chrome::{
    BottomTab, EditorDimension, InspectorTab, PanelVisibility, TransformSpace, Workspace,
};
use crate::console::{ConsoleFilter, ConsoleLog};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::dirty::{DirtyState, PendingAction};
use crate::error::EditorError;
use crate::gizmo::GizmoHandle;
use crate::hierarchy::{self, HierarchyState};
use crate::history::History;
use crate::import::AssetImporter;
use crate::inspector;
use crate::output::OutputLog;
use crate::overlays::OverlayToggles;
use crate::play::PlayState;
use crate::preview::PreviewCache;
use crate::profiler::FrameProfiler;
use crate::session::{EditorSession, SESSION_VERSION};
use crate::terrain_tools::{TerrainStroke, TerrainToolState};

/// Where the editor's "Save Scene" / "Open Scene" actions read and write,
/// relative to the editor's working directory.
pub const DEFAULT_SCENE_PATH: &str = "scene.ron";

/// Owned by the editor binary (`editor/src/main.rs`), passed to
/// [`crate::EditorShell::run_frame`] by mutable reference each frame —
/// mirrors how [`crate::Viewport`] is borrowed rather than owned by the
/// shell itself.
pub struct EditorState {
    /// Bounded AI conversation state; transport is supplied by the host.
    pub ai_chat: editor_ai::ChatSession,
    /// Active AI intent mode used to constrain future tool dispatch.
    pub ai_mode: editor_ai::AiMode,
    /// Unsubmitted text in the AI Assistant composer.
    pub ai_input: String,
    /// Bounded plan/action history shown in the AI Assistant panel.
    pub action_log: editor_tools::ActionLog,
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
    /// The Asset Browser's own state: grouping, the folder imports land
    /// in, and its pending new-folder request. The grouping is persisted
    /// per project via [`EditorSession`].
    pub asset_browser: AssetBrowser,
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
    /// The gizmo handle (move arrow, rotation ring or scale square) being
    /// dragged, if a drag is in progress — `None` the rest of the time.
    pub dragging_handle: Option<GizmoHandle>,
    /// Where [`EditorState::save_scene`]/[`EditorState::load_scene`] read
    /// and write. Defaults to [`DEFAULT_SCENE_PATH`].
    pub scene_path: PathBuf,
    /// Undo/redo stack of world snapshots — driven by the editor shell
    /// around each frame's UI pass (see [`History`]).
    pub history: History,
    /// Rolling CPU frame-time history, fed by the editor binary's loop
    /// and shown in the Profiler panel.
    pub profiler: FrameProfiler,
    /// Play-mode: run the scene (via the play schedule) and rewind on
    /// stop. See [`PlayState`].
    pub play: PlayState,
    /// A project the user asked to open, recorded by the File menu and
    /// carried out by the shell binary — which owns the GPU resources
    /// and language server a swap also has to reset. See
    /// [`crate::ProjectRequest`].
    pub project_request: Option<crate::ProjectRequest>,
    /// Recently opened projects, for the File ▸ Open Recent menu. Loaded
    /// from the user's config at startup by the shell binary.
    pub recent_projects: crate::RecentProjects,
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
    /// The scene as it is on disk — captured on load and save. The scene
    /// only counts as unsaved while the world differs from this, so an
    /// edit that is undone, or a change set back by hand, is not reported.
    /// `None` when the world has no file behind it yet (a new scene, or a
    /// scene file that failed to load).
    saved_scene: Option<Scene>,
    /// A guarded action (quit / new scene / revert) waiting on the
    /// "unsaved changes" dialog. `None` when no dialog is open.
    pub pending_action: Option<PendingAction>,
    /// Which of the project's `BuildConfiguration`s the toolbar has
    /// selected, as an index into
    /// [`engine_project::Project::configurations`]. Persisted per user in
    /// the session — the definitions are the project's, the choice is
    /// yours. May be stale after someone edits `project.ron`, so read it
    /// through [`EditorState::selected_configuration`].
    pub configuration_index: usize,
    /// Set by File ▸ Build & Export…: the folder to stage the export
    /// into. Taken by the shell binary, which owns the build thread.
    pub export_request: Option<PathBuf>,
    /// File-menu request to pin this Rust project to the bundled SDK.
    pub sdk_upgrade_request: bool,
    /// Result shown in a dismissible dialog after the host handles that request.
    pub sdk_upgrade_message: Option<String>,
    /// Whether an export build is running. The menu item is disabled
    /// while it is — two cargo builds writing one `target/` is a way to
    /// lose an afternoon.
    pub export_running: bool,
    /// Which Scene-view debug overlays are on. Persisted per project in
    /// the session.
    pub overlays: OverlayToggles,
    /// The gizmo coordinate space shown in the toolbar (display-only for
    /// now — the gizmo still uses world axes).
    pub transform_space: TransformSpace,
    /// The Scene View tool chosen in the toolbar.
    pub view_tool: crate::ViewTool,
    /// Space is held over the Scene View: Hand applies until it is released.
    pub temporary_hand: bool,
    /// 3D vs 2D authoring: switches the viewport camera to an
    /// orthographic front view and constrains the gizmo to the screen
    /// plane. In-memory only (not persisted in the session yet).
    pub dimension: EditorDimension,
    /// Terrain and vegetation authoring controls used by the Scene View.
    pub terrain_tools: TerrainToolState,
    /// Enables brush interaction instead of the transform gizmo.
    pub terrain_sculpt_active: bool,
    /// Paints sprite cells on a selected tilemap in the 2D viewport.
    pub tile_paint_active: bool,
    /// Erases cells instead of applying the tilemap's brush cell.
    pub tile_erase_active: bool,
    /// Whether the sun is being aimed this frame (the Scene View's
    /// Environment menu is open), so the viewport draws its direction.
    /// Set each frame by the Scene View footer.
    pub aiming_sun: bool,
    /// In-progress viewport stroke; no disk writes until pointer release.
    pub terrain_stroke: Option<TerrainStroke>,
    /// Finished stroke waiting for the host's asset write/import pass.
    pub terrain_sculpt_request: Option<TerrainStroke>,
    /// Scene View navigation for the 3D and 2D modes.
    pub scene_cameras: crate::SceneCameras,
    /// A terrain model rewritten by undo/redo, waiting for the host to
    /// re-import it and refresh the meshes that use it.
    pub terrain_refresh_request: Option<std::path::PathBuf>,
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
    /// Render-target size requested by the visible viewport panel this
    /// frame, in physical pixels. The host applies it after presenting.
    pub viewport_size_request: Option<(u32, u32)>,
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
    /// Per-user Studio settings (Settings window).
    pub settings: crate::storage::StudioSettings,
    /// Whether the Settings window is open.
    pub settings_open: bool,
    /// Storage measurements, cleaning and the automatic limits.
    pub storage: crate::storage::StorageView,
    /// The game shown in the Scene View while Play previews it.
    pub game_view: crate::game_view::GameView,
    /// The hierarchy panel's search text and in-progress inline rename.
    pub hierarchy: HierarchyState,
    /// Project search query, results, and pending jump.
    pub search: crate::search::SearchState,
    /// A component type name whose "Open Script" button was clicked in
    /// the Inspector; the host resolves it to a source location and
    /// opens it. Drained each frame.
    pub open_script_request: Option<String>,
    /// An entity the user asked to save as a `.prefab` (hierarchy "Make
    /// Prefab"). The host writes the file and refreshes the asset list.
    /// Drained each frame.
    pub create_prefab_request: Option<Entity>,
    /// Set by the Terrain menu; the host writes an editable terrain mesh
    /// and refreshes imports after the UI frame.
    pub create_terrain_request: bool,
    /// Set when the user picked `Assets ▸ Import…`. The host opens the
    /// file dialog, because the dialog blocks and the copy needs the
    /// project directory — neither belongs in a menu closure that only
    /// has `&mut egui::Ui`.
    pub import_request: bool,
    /// Files dropped on the window since the last frame, awaiting import.
    pub dropped_files: Vec<std::path::PathBuf>,
    /// Set when the user picked `Assets ▸ Refresh`.
    pub rescan_request: bool,
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
            ai_chat: editor_ai::ChatSession::default(),
            ai_mode: editor_ai::AiMode::default(),
            ai_input: String::new(),
            action_log: editor_tools::ActionLog::default(),
            world,
            selected_entity: None,
            secondary_selection: Vec::new(),
            assets,
            asset_index: AssetIndex::new(),
            importer: AssetImporter::new(),
            selected_asset: None,
            asset_browser: AssetBrowser::default(),
            preview: PreviewCache::new(),
            console,
            console_filter: ConsoleFilter::default(),
            diagnostics: Diagnostics::new(),
            output: OutputLog::new(),
            dragging_handle: None,
            scene_path,
            history: History::new(),
            profiler: FrameProfiler::new(),
            play: PlayState::new(),
            project_request: None,
            recent_projects: crate::RecentProjects::new(),
            project,
            workspace: Workspace::default(),
            panels: PanelVisibility::default(),
            workspace_layouts: Workspace::ALL.map(Workspace::visibility),
            dirty: DirtyState::default(),
            saved_scene: None,
            pending_action: None,
            configuration_index: 0,
            overlays: OverlayToggles::default(),
            export_request: None,
            sdk_upgrade_request: false,
            sdk_upgrade_message: None,
            export_running: false,
            transform_space: TransformSpace::default(),
            view_tool: crate::ViewTool::default(),
            temporary_hand: false,
            dimension: EditorDimension::default(),
            terrain_tools: TerrainToolState::default(),
            terrain_sculpt_active: false,
            tile_paint_active: false,
            tile_erase_active: false,
            aiming_sun: false,
            terrain_stroke: None,
            terrain_sculpt_request: None,
            terrain_refresh_request: None,
            scene_cameras: crate::SceneCameras::default(),
            batch_edit: false,
            bottom_tab: BottomTab::default(),
            inspector_tab: InspectorTab::default(),
            exit_requested: false,
            file_open_request: None,
            problem_jump: None,
            problem_selected: None,
            code_panel_rect: None,
            viewport_size_request: None,
            code_panel_visible: false,
            prev_code_panel_rect: None,
            code_panel_resizing: false,
            code_save_requested: false,
            cursor_line: 1,
            cursor_column: 1,
            #[cfg(feature = "terminal")]
            terminal: None,
            settings: crate::storage::StudioSettings::default(),
            settings_open: false,
            storage: crate::storage::StorageView::default(),
            game_view: crate::game_view::GameView::default(),
            hierarchy: HierarchyState::default(),
            search: crate::search::SearchState::default(),
            open_script_request: None,
            create_prefab_request: None,
            create_terrain_request: false,
            import_request: false,
            dropped_files: Vec::new(),
            rescan_request: false,
            recovered_from_autosave: false,
        }
    }

    /// Flags the scene as having unsaved edits. Called after any settled
    /// undo step or scene-mutating menu action.
    pub fn mark_dirty(&mut self) {
        self.dirty.mark_scene();
    }

    /// Re-checks the scene's unsaved flag against the scene on disk:
    /// clears it (and the crash autosave) when the world matches the
    /// saved file again, sets it when it doesn't. Without a saved file
    /// the flag is left as it is.
    ///
    /// Call after edits settle and before asking whether anything is
    /// unsaved; it captures the whole world, so not every frame.
    pub fn sync_scene_dirty(&mut self) {
        // In Play the world is the running game, not the edited scene.
        if self.play.is_playing() {
            return;
        }
        let Some(saved) = &self.saved_scene else {
            return;
        };
        if Scene::from_world(&mut self.world) == *saved {
            if self.dirty.scene() {
                self.dirty.clear_scene();
                self.clear_autosave();
            }
        } else {
            self.dirty.mark_scene();
        }
    }

    /// Whether anything is unsaved, with the scene re-checked against
    /// its file first (see [`EditorState::sync_scene_dirty`]). Use this
    /// for the close guard and the "unsaved changes" prompt.
    pub fn has_unsaved_changes(&mut self) -> bool {
        self.sync_scene_dirty();
        self.dirty.any()
    }

    /// Drops the crash autosave because the user chose to discard the
    /// scene's unsaved edits — otherwise the next launch would recover
    /// the very edits they threw away.
    pub fn discard_unsaved_scene(&mut self) {
        self.clear_autosave();
    }

    /// Applies a scene mutation from the shared Studio tool schema through
    /// the same snapshot-based undo path used by panel edits.
    pub fn apply_tool_scene_edit(
        &mut self,
        request: &editor_tools::ToolRequest,
    ) -> Result<(), String> {
        self.history
            .begin_frame(&mut self.world, self.selected_entity);
        match request {
            editor_tools::ToolRequest::CreateEntity { name } => {
                self.world.spawn((
                    engine_ecs::components::Name::new(name),
                    engine_ecs::components::Transform::default(),
                ));
            }
            editor_tools::ToolRequest::SetTransform {
                entity,
                translation,
            } => {
                let target = engine_ecs::prelude::Entity::from_bits(*entity);
                let Some(mut transform) = self
                    .world
                    .get_mut::<engine_ecs::components::Transform>(target)
                else {
                    return Err(format!("entity {entity} was not found"));
                };
                transform.0.translation = glam::Vec3::from_array(*translation);
            }
            _ => return Err("request is not a scene mutation".into()),
        }
        if self.history.settle(&mut self.world, self.selected_entity) {
            self.mark_dirty();
        }
        Ok(())
    }

    /// Reverts the most recent edit (world or terrain stroke).
    pub fn undo(&mut self) {
        let result = self
            .history
            .undo(&mut self.world, &mut self.selected_entity);
        self.follow_history_change(result);
    }

    /// Re-applies the most recently undone edit.
    pub fn redo(&mut self) {
        let result = self
            .history
            .redo(&mut self.world, &mut self.selected_entity);
        self.follow_history_change(result);
    }

    /// A world restore invalidates selection-derived state; a terrain restore
    /// asks the host to reload that terrain's mesh; a failure is shown.
    fn follow_history_change(
        &mut self,
        result: Result<Option<crate::HistoryChange>, crate::HistoryError>,
    ) {
        match result {
            Ok(Some(crate::HistoryChange::World)) => {
                self.secondary_selection.clear();
                self.dragging_handle = None;
                // Undoing back to the saved state makes the scene clean.
                self.sync_scene_dirty();
            }
            Ok(Some(crate::HistoryChange::Terrain { model_path })) => {
                self.terrain_refresh_request = Some(model_path);
            }
            Ok(None) => {}
            Err(error) => self.asset_browser.operation_error = Some(error.to_string()),
        }
    }

    /// Replaces the world with a fresh empty scene: clears the selection
    /// and any in-progress gizmo drag, resets the undo history, and marks
    /// the scene dirty (the empty world is not yet on disk). The scene
    /// path is unchanged, so the next save writes over the same file.
    pub fn new_scene(&mut self) {
        self.world = World::new();
        self.clear_selection();
        self.dragging_handle = None;
        self.terrain_stroke = None;
        self.terrain_sculpt_request = None;
        self.tile_paint_active = false;
        self.history = History::new();
        self.saved_scene = None;
        self.dirty.mark_scene();
    }

    /// Adopts `project`, discarding everything that belonged to the
    /// previous one.
    ///
    /// Resets the world, selection, undo history, asset list, asset
    /// index and import cache, and points `scene_path` at the new
    /// project's main scene. **Does not** load that scene or touch GPU
    /// resources: the caller loads the scene (so it can report a
    /// readable error) and the shell binary rebuilds its render assets,
    /// language server and code editor.
    ///
    /// Anything left un-reset here is a bug with a memorable symptom —
    /// the new project rendering the old project's meshes, or an undo
    /// that reaches back into a world that no longer exists.
    pub fn adopt_project(&mut self, project: Project) {
        self.scene_path = project.main_scene_path();
        self.project = project;

        self.world = World::new();
        self.clear_selection();
        self.dragging_handle = None;
        self.terrain_stroke = None;
        self.terrain_sculpt_request = None;
        self.terrain_sculpt_active = false;
        self.tile_paint_active = false;
        self.tile_erase_active = false;
        self.history = History::new();

        // The import cache is keyed by path, and paths from the old
        // project mean nothing here.
        self.assets = Vec::new();
        let view = self.asset_browser.view;
        let preview_open = self.asset_browser.preview_open;
        self.asset_browser = AssetBrowser::default();
        self.asset_browser.view = view;
        self.asset_browser.preview_open = preview_open;
        self.asset_index = AssetIndex::new();
        self.importer = AssetImporter::new();
        self.selected_asset = None;
        self.preview = PreviewCache::new();

        // Nothing is unsaved in a project just opened, and a pending
        // prompt about the *old* project's changes would be nonsense.
        self.dirty = DirtyState::default();
        self.saved_scene = None;
        self.pending_action = None;
        self.project_request = None;
        self.sdk_upgrade_request = false;
        self.sdk_upgrade_message = None;
        self.play = PlayState::new();

        self.recent_projects.push(self.project.root());
        let root = self.project.root().to_path_buf();
        self.storage.opened(&root);
    }

    /// Runs the storage limits and applies background results (see
    /// [`crate::storage::StorageView::tick`]).
    pub fn tick_storage(&mut self) {
        let root = self.project.root().to_path_buf();
        self.storage
            .tick(self.settings.storage, &root, &self.output);
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

    /// The build configuration the toolbar has selected, falling back to
    /// the project's first if the remembered index no longer exists.
    pub fn selected_configuration(&self) -> Option<&engine_project::BuildConfiguration> {
        self.project.configuration(self.configuration_index)
    }

    /// Restores editor-only state from a loaded [`EditorSession`]: the
    /// workspace, the exact panel visibility (which may differ from the
    /// workspace preset), and — if its file still exists inside the
    /// project — the last-open scene path.
    pub fn apply_session(&mut self, session: EditorSession) {
        self.workspace = session.workspace;
        self.panels = session.panels;
        self.asset_browser.view = session.asset_view;
        self.asset_browser.preview_open = session.preview_open;
        self.overlays = session.overlays;
        self.hierarchy.open = session.hierarchy_open;
        self.asset_browser.open_folders = session.asset_folders_open;
        match &session.scene_view {
            Some(view) => {
                self.scene_cameras = crate::SceneCameras::from_saved(view);
                self.dimension = if view.mode_2d {
                    EditorDimension::Mode2D
                } else {
                    EditorDimension::Mode3D
                };
            }
            None => {
                self.scene_cameras = crate::SceneCameras::default();
                self.dimension = EditorDimension::default();
            }
        }
        // Clamped, not trusted: the session is per-user state that may
        // be older than the project's configuration list.
        self.configuration_index = session
            .configuration_index
            .min(self.project.configurations().len().saturating_sub(1));
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
            asset_view: self.asset_browser.view,
            preview_open: self.asset_browser.preview_open,
            configuration_index: self.configuration_index,
            overlays: self.overlays,
            hierarchy_open: self.hierarchy.open.clone(),
            asset_folders_open: self.asset_browser.open_folders.clone(),
            scene_view: Some(self.scene_cameras.saved(self.dimension.is_2d())),
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
        self.saved_scene = Some(scene);
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
        self.sync_scene_dirty();
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
                // An autosave that matches the scene file holds nothing
                // to recover — drop it rather than flag the scene.
                self.sync_scene_dirty();
                self.recovered_from_autosave = self.dirty.scene();
                Ok(self.recovered_from_autosave)
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
    /// A mesh asset also gets a [`MeshSource`], which is what makes it
    /// *render*: the per-frame reconciliation pass
    /// ([`crate::resolve_pending`]) turns that reference into a live
    /// `MeshRenderer` as soon as the importer has the asset decoded, and
    /// [`engine_scene::capture_renderables`] writes it back out on save.
    /// Until then the entity draws as a wireframe placeholder.
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
        // A mesh reference needs both halves. There is no separate
        // material asset yet, so the mesh's own id stands in for it —
        // the glTF's embedded material travels with the mesh.
        let mesh_source = self
            .asset_index
            .id_for(asset_path)
            .filter(|_| {
                matches!(
                    crate::assets::AssetKind::from_path(asset_path),
                    crate::assets::AssetKind::Mesh
                )
            })
            .map(|id| MeshSource::new(id.to_string(), id.to_string()));

        let mut entity = self
            .world
            .spawn((Name::new(name), TransformComponent::default(), source));
        if let Some(mesh_source) = mesh_source {
            entity.insert(mesh_source);
        }
        if matches!(
            crate::assets::AssetKind::from_path(asset_path),
            crate::assets::AssetKind::Texture
        ) && let Some(id) = self.asset_index.id_for(asset_path)
        {
            entity.insert((
                SpriteSource {
                    atlas: id.to_string(),
                    region: "full".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                Sprite::new(
                    glam::Vec2::ONE,
                    engine_renderer::UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
            ));
        }
        entity.id()
    }

    /// Selected image eligible for sprite creation, if it has a stable asset ID.
    pub fn selected_texture_asset(&self) -> Option<&Path> {
        self.selected_asset.as_deref().filter(|path| {
            crate::assets::AssetKind::from_path(path) == crate::assets::AssetKind::Texture
                && self.asset_index.id_for(path).is_some()
        })
    }

    /// Selected sound file, if it has a stable asset ID.
    pub fn selected_audio_asset(&self) -> Option<&Path> {
        self.selected_asset.as_deref().filter(|path| {
            crate::assets::AssetKind::from_path(path) == crate::assets::AssetKind::Audio
                && self.asset_index.id_for(path).is_some()
        })
    }

    /// The scene's environment settings (empty when none are set).
    pub fn environment(&self) -> engine_scene::EnvironmentData {
        self.world
            .get_resource::<engine_scene::SceneEnvironment>()
            .map(|carrier| carrier.0.clone())
            .unwrap_or_default()
    }

    /// Replaces the scene's environment settings and marks the scene dirty.
    pub fn set_environment(&mut self, environment: engine_scene::EnvironmentData) {
        if environment.is_empty() {
            self.world
                .remove_resource::<engine_scene::SceneEnvironment>();
        } else {
            self.world
                .insert_resource(engine_scene::SceneEnvironment(environment));
        }
        self.mark_dirty();
    }

    /// Adds the selected sound file as a looping ambient sound for the scene.
    pub fn add_ambient_sound_from_selection(&mut self) -> Result<(), String> {
        let path = self
            .selected_audio_asset()
            .ok_or("Select an imported WAV sound in Project Assets")?;
        let id = self
            .asset_index
            .id_for(path)
            .ok_or("The selected sound has no asset ID yet")?
            .to_string();
        let mut environment = self.environment();
        if environment
            .ambient_sounds
            .iter()
            .any(|ambient| ambient.sound.id == id)
        {
            return Err("That sound is already an ambient sound".into());
        }
        if environment.ambient_sounds.len() >= engine_scene::EnvironmentData::MAX_AMBIENT_SOUNDS {
            return Err(format!(
                "A scene can have at most {} ambient sounds",
                engine_scene::EnvironmentData::MAX_AMBIENT_SOUNDS
            ));
        }
        environment
            .ambient_sounds
            .push(engine_scene::AmbientSoundData {
                sound: engine_scene::AssetRef { id },
                gain: 1.0,
            });
        self.set_environment(environment);
        Ok(())
    }

    /// Creates a sparse tilemap from the selected imported image.
    pub fn create_tilemap_from_selected_image(&mut self) -> Result<Entity, String> {
        let image = self
            .selected_texture_asset()
            .ok_or("Select an imported PNG or JPEG in Project Assets")?;
        let id = self
            .asset_index
            .id_for(image)
            .ok_or("The selected image has no asset ID")?;
        let parent = hierarchy::spawn_entity(&mut self.world, "Tilemap", Vec3::ZERO);
        self.world
            .entity_mut(parent)
            .insert(engine_scene::SceneTilemap(engine_scene::TilemapData::new(
                engine_scene::AssetRef { id: id.to_string() },
            )));
        self.select_only(parent);
        self.dimension = EditorDimension::Mode2D;
        self.terrain_sculpt_active = false;
        self.tile_paint_active = true;
        self.tile_erase_active = false;
        self.mark_dirty();
        Ok(parent)
    }

    /// Paints or erases one cell of the selected tilemap. Repeating the same
    /// brush on the same cell is a no-op, so a drag does not spawn duplicates.
    pub fn paint_tile_cell(&mut self, x: i32, y: i32, erase: bool) -> Result<bool, String> {
        let parent = self.selected_entity.ok_or("Select a tilemap first")?;
        let map = self
            .world
            .get::<engine_scene::SceneTilemap>(parent)
            .ok_or("Select a tilemap first")?
            .0
            .clone();
        if x.unsigned_abs() > 10_000 || y.unsigned_abs() > 10_000 {
            return Err("Tile position is outside the supported editing area".into());
        }
        map.validate()
            .map_err(|error| format!("Tilemap settings are invalid: {error}"))?;
        let center = Vec3::new(
            (x as f32 + 0.5) * map.cell_size[0],
            (y as f32 + 0.5) * map.cell_size[1],
            0.0,
        );
        if !center.is_finite() {
            return Err("Tile position is not finite".into());
        }
        let existing = self.world.get::<Children>(parent).and_then(|children| {
            children
                .iter()
                .find(|&&child| {
                    self.world.get::<SpriteSource>(child).is_some()
                        && self
                            .world
                            .get::<TransformComponent>(child)
                            .is_some_and(|transform| transform.0.translation == center)
                })
                .copied()
        });
        if erase {
            if let Some(existing) = existing {
                self.world.despawn(existing);
                self.mark_dirty();
                return Ok(true);
            }
            return Ok(false);
        }
        let region = engine_scene::GridRegion {
            columns: map.columns,
            rows: map.rows,
            column: map.brush_column,
            row: map.brush_row,
        }
        .name();
        let brush_collider = crate::tilemap::brush_collider(map.cell_size);
        if let Some(existing) = existing {
            let mut changed = false;
            {
                let Some(mut source) = self.world.get_mut::<SpriteSource>(existing) else {
                    return Err("Tile no longer has an image".into());
                };
                if source.region != region || source.atlas != map.atlas.id {
                    source.region = region;
                    source.atlas = map.atlas.id;
                    changed = true;
                }
            }
            let current = self.world.get::<engine_scene::SceneCollider>(existing);
            if map.brush_collision && current.is_none() {
                self.world
                    .entity_mut(existing)
                    .insert(engine_scene::SceneCollider(brush_collider));
                changed = true;
            } else if !map.brush_collision
                && current.is_some_and(|collider| collider.0 == brush_collider)
            {
                self.world
                    .entity_mut(existing)
                    .remove::<engine_scene::SceneCollider>();
                changed = true;
            }
            if changed {
                self.mark_dirty();
            }
            return Ok(changed);
        }
        if self
            .world
            .get::<Children>(parent)
            .is_some_and(|children| children.len() >= 20_000)
        {
            return Err("Tilemap has reached the 20,000-cell editing limit".into());
        }
        let tile = self
            .world
            .spawn((
                Name::new(format!("Tile ({x}, {y})")),
                TransformComponent::from(engine_utils::Transform::from_translation(center)),
                ChildOf(parent),
                SpriteSource {
                    atlas: map.atlas.id,
                    region,
                    size: map.cell_size,
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                Sprite::new(
                    Vec2::from_array(map.cell_size),
                    engine_renderer::UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
            ))
            .id();
        if map.brush_collision {
            self.world
                .entity_mut(tile)
                .insert(engine_scene::SceneCollider(brush_collider));
        }
        self.mark_dirty();
        Ok(true)
    }

    /// Returns the current read-only reverse dependency report for scene assets.
    pub fn asset_dependencies(&mut self) -> Vec<crate::dependencies::AssetDependency> {
        crate::dependencies::scan(&mut self.world, &self.asset_index)
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
    /// scene or after an asset move so a reference to a renamed/moved asset
    /// still points at the right file. Marks the scene dirty when it changes anything, so a
    /// save persists the healed reference.
    pub fn heal_asset_sources(&mut self) {
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
        self.terrain_stroke = None;
        self.terrain_sculpt_request = None;
        self.clear_selection();
        self.saved_scene = Some(Scene::from_world(&mut self.world));
        self.dirty.clear_scene();
        self.heal_asset_sources();
        Ok(())
    }

    /// Loads the selected terrain's source data after validating both paths
    /// against the project root. A non-terrain selection is a clean error.
    pub fn begin_terrain_stroke(&self) -> Result<TerrainStroke, String> {
        let entity = self.selected_entity.ok_or("Select a terrain first")?;
        let source = self
            .world
            .get::<AssetSource>(entity)
            .ok_or("Selected object has no terrain asset")?;
        let relative = PathBuf::from(&source.path);
        if relative
            .extension()
            .is_none_or(|extension| extension != "gltf")
        {
            return Err("Selected object is not a terrain mesh".into());
        }
        let model = self
            .project
            .resolve(&Path::new("assets").join(&relative))
            .map_err(|error| error.to_string())?;
        let authored = self
            .project
            .resolve(&Path::new("assets").join(relative.with_extension("terrain")))
            .map_err(|error| error.to_string())?;
        if !model.is_file() {
            return Err("Terrain mesh is missing".into());
        }
        let asset = crate::terrain_tools::TerrainAsset::load(&authored)
            .map_err(|error| format!("Could not load terrain source: {error}"))?;
        let (heightmap, _) = asset.into_parts().map_err(|error| error.to_string())?;
        Ok(TerrainStroke {
            entity,
            relative_model: relative,
            heightmap,
            centers: Vec::new(),
        })
    }

    /// Bakes a selected model into one instanced group on the selected terrain.
    pub fn scatter_selected_mesh_on_terrain(&mut self) -> Result<usize, String> {
        let model = self
            .selected_asset
            .as_deref()
            .ok_or("Select a model in Project Assets first")?;
        if crate::assets::AssetKind::from_path(model) != crate::assets::AssetKind::Mesh {
            return Err("Select a glTF or GLB model in Project Assets".into());
        }
        let model_id = self
            .asset_index
            .id_for(model)
            .ok_or("The selected model has not been imported yet")?;
        let model_path = self
            .project
            .resolve(&Path::new("assets").join(model))
            .map_err(|error| error.to_string())?;
        if !model_path.is_file() {
            return Err("The selected model file is missing".into());
        }
        let terrain = self.begin_terrain_stroke()?;
        let size = terrain.heightmap.size();
        let mut controls = self.terrain_tools.clone();
        controls.sanitize();
        let cells_per_side = (size / controls.scatter_spacing).ceil().max(1.0) as u64;
        if cells_per_side.saturating_mul(cells_per_side) > 20_000 {
            return Err(
                "Foliage area is too dense; increase Spacing (maximum 20,000 cells)".into(),
            );
        }
        let area = engine_renderer::ScatterArea::new(Vec2::splat(-size * 0.5), Vec2::splat(size));
        let terrain_world = hierarchy::world_transform(&self.world, terrain.entity);
        let instances: Vec<_> = controls
            .scatter_preview(area, Some(&terrain.heightmap))
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|pose| engine_scene::TransformData::from(terrain_world.mul_transform(&pose)))
            .collect();
        if instances.is_empty() {
            return Err("No foliage was placed; increase Density".into());
        }
        let authored_path = self
            .project
            .resolve(&Path::new("assets").join(terrain.relative_model.with_extension("terrain")))
            .map_err(|error| error.to_string())?;
        crate::terrain_tools::TerrainAsset::from_heightmap(&terrain.heightmap, &controls)
            .save(&authored_path)
            .map_err(|error| format!("Could not save foliage settings: {error}"))?;

        let count = instances.len();
        let name = model
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Model");
        let id = model_id.to_string();
        let group = self
            .world
            .spawn((
                Name::new(format!("{name} Foliage")),
                engine_scene::SceneFoliage(engine_scene::FoliageData {
                    mesh: engine_scene::AssetRef { id: id.clone() },
                    material: engine_scene::AssetRef { id },
                    instances,
                }),
            ))
            .id();
        self.select_only(group);
        self.mark_dirty();
        Ok(count)
    }

    /// Queues an authored stroke only if its terrain is still selected.
    pub fn finish_terrain_stroke(&mut self) {
        if let Some(stroke) = self.terrain_stroke.take()
            && self.selected_entity == Some(stroke.entity)
            && !stroke.centers.is_empty()
        {
            self.terrain_sculpt_request = Some(stroke);
        }
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

    /// Puts the selection into a new group (see
    /// [`hierarchy::group_entities`]), selects it and starts renaming it in
    /// the Hierarchy. Returns whether a group was made.
    pub fn group_selection(&mut self) -> bool {
        let selection = self.selection();
        let Some(group) = hierarchy::group_entities(&mut self.world, &selection) else {
            return false;
        };
        self.select_only(group);
        self.hierarchy.start_rename(&self.world, group);
        true
    }

    /// Ungroups every selected group (see [`hierarchy::ungroup`]) and
    /// selects what was inside. Returns whether anything was ungrouped.
    pub fn ungroup_selection(&mut self) -> bool {
        let freed: Vec<Entity> = self
            .selection()
            .into_iter()
            .filter_map(|entity| hierarchy::ungroup(&mut self.world, entity))
            .flatten()
            .collect();
        let Some((&first, rest)) = freed.split_first() else {
            return false;
        };
        self.selected_entity = Some(first);
        self.secondary_selection = rest.to_vec();
        true
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

    /// The Scene View tool in effect: Hand while Space is held, else the
    /// toolbar choice.
    pub fn active_view_tool(&self) -> crate::ViewTool {
        if self.temporary_hand {
            crate::ViewTool::Hand
        } else {
            self.view_tool
        }
    }

    /// The Scene View camera for the current 2D/3D mode.
    pub fn scene_camera(&self) -> &crate::SceneCamera {
        self.scene_cameras.active(self.dimension.is_2d())
    }

    /// The sun's direction arrow for the viewport while the sun is being
    /// aimed: ending at the centre of the view and sized to it, so it is in
    /// sight at any zoom. `None` the rest of the time.
    pub fn sun_arrow(&self) -> Option<crate::overlays::SunArrow> {
        self.aiming_sun.then(|| {
            let camera = self.scene_camera();
            crate::overlays::SunArrow {
                at: camera.focus,
                length: (camera.distance * 0.25).clamp(1.0, 500.0),
            }
        })
    }

    /// Points both Scene View cameras (3D and 2D) at the whole scene,
    /// keeping their viewing angle, so a scene opens showing all of it.
    /// Leaves the cameras alone for an empty scene.
    pub fn frame_scene(&mut self) {
        let Some((center, radius)) = self.scene_bounds() else {
            return;
        };
        for two_d in [false, true] {
            self.scene_cameras.stop_glide(two_d);
            self.scene_cameras.active_mut(two_d).frame(center, radius);
        }
    }

    /// The sphere [`EditorState::frame_scene`] fits on screen, or `None` for
    /// an empty scene.
    fn scene_bounds(&mut self) -> Option<(Vec3, f32)> {
        // Only placed objects count: not screen-space game UI, and not the
        // ECS's own internal entities, which have no transform.
        let mut query = self.world.query_filtered::<engine_ecs::prelude::Entity, (
            bevy_ecs::query::With<engine_ecs::components::Transform>,
            bevy_ecs::query::Without<engine_ecs::prelude::ChildOf>,
            bevy_ecs::query::Without<engine_scene::SceneUi>,
        )>();
        let roots: Vec<_> = query.iter(&self.world).collect();
        let (center, radius) = selection_bounds(&self.world, &roots)?;
        // The sphere around the scene's bounding box is generous (its
        // corners are mostly empty), so fit a smaller one: the scene's main
        // area fills the view instead of floating in the middle of it.
        Some((center, radius * SCENE_FIT))
    }

    /// Moves the Scene View camera to fit every selected object and its
    /// children, or the whole scene (as when a project first opens) when
    /// nothing is selected. Returns `false` when there is nothing to frame.
    pub fn frame_selection(&mut self) -> bool {
        let roots: Vec<_> = self
            .selected_entity
            .into_iter()
            .chain(self.secondary_selection.iter().copied())
            .collect();
        let bounds = if roots.is_empty() {
            self.scene_bounds()
        } else {
            selection_bounds(&self.world, &roots)
        };
        let Some((center, radius)) = bounds else {
            return false;
        };
        let two_d = self.dimension.is_2d();
        let mut target = *self.scene_cameras.active(two_d);
        target.frame(center, radius);
        self.scene_cameras.glide_to(two_d, target);
        true
    }

    /// The selected entity's world position, if it's selected and has a
    /// `Transform`: where the Scene View's gizmo draws. World, not local:
    /// an object inside a group is drawn where it is, not at its offset
    /// from the group. `None` hides the gizmo, including for a locked
    /// object.
    pub fn gizmo_origin(&self) -> Option<Vec3> {
        let entity = self.selected_entity?;
        // A locked object cannot be moved, so it shows no handles to grab.
        if self
            .world
            .get::<engine_ecs::components::Lock>(entity)
            .is_some()
        {
            return None;
        }
        inspector::transform_of(&self.world, entity)?;
        Some(hierarchy::world_transform(&self.world, entity).translation)
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

/// A bounding sphere around `roots` and all their descendants, from world
/// positions plus a per-object size (sprite size, else largest scale axis).
/// How much of the scene's bounding sphere [`EditorState::frame_scene`]
/// fits on screen.
const SCENE_FIT: f32 = 0.75;

fn selection_bounds(
    world: &engine_ecs::prelude::World,
    roots: &[engine_ecs::prelude::Entity],
) -> Option<(Vec3, f32)> {
    use engine_ecs::prelude::Children;

    let mut objects = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(entity) = stack.pop() {
        if world.get_entity(entity).is_err() || objects.len() >= 10_000 {
            continue;
        }
        let transform = hierarchy::world_transform(world, entity);
        let extent = world
            .get::<engine_ecs::components::SpriteSource>(entity)
            .map_or(1.0, |sprite| sprite.size[0].max(sprite.size[1]) * 0.5);
        let radius = extent * transform.scale.max_element().abs();
        objects.push((transform.translation, radius));
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
    }
    let first = objects.first()?;
    let (mut min, mut max) = (first.0, first.0);
    for (position, radius) in &objects {
        min = min.min(*position - Vec3::splat(*radius));
        max = max.max(*position + Vec3::splat(*radius));
    }
    let center = (min + max) * 0.5;
    Some((center, (max - min).length() * 0.5))
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
    fn solid_tile_brush_saves_a_fixed_cell_collider() {
        use engine_ecs::prelude::Children;
        use engine_scene::{
            AssetRef, BodyKind, ColliderShape, SceneCollider, SceneTilemap, TilemapData,
        };

        let project = temp_project();
        let mut state = editor_state(&project);
        let parent = hierarchy::spawn_entity(&mut state.world, "Tilemap", Vec3::ZERO);
        let mut map = TilemapData::new(AssetRef {
            id: uuid::Uuid::new_v4().to_string(),
        });
        map.cell_size = [2.0, 3.0];
        map.brush_collision = true;
        state.world.entity_mut(parent).insert(SceneTilemap(map));
        state.select_only(parent);

        assert!(state.paint_tile_cell(2, -1, false).unwrap());
        let tile = state
            .world
            .get::<Children>(parent)
            .unwrap()
            .iter()
            .next()
            .copied()
            .unwrap();
        let collider = state.world.get::<SceneCollider>(tile).unwrap().0.clone();
        assert_eq!(collider.body, BodyKind::Fixed);
        assert_eq!(
            collider.shape,
            ColliderShape::Cuboid {
                half_extents: [1.0, 1.5, 0.05]
            }
        );
        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert!(
            saved
                .entities
                .iter()
                .any(|entity| entity.name.as_deref() == Some("Tile (2, -1)")
                    && entity.collider.as_ref() == Some(&collider))
        );
    }

    #[test]
    fn repainting_a_tile_changes_its_solid_state_without_duplicating_it() {
        use engine_ecs::prelude::Children;
        use engine_scene::{AssetRef, SceneCollider, SceneTilemap, TilemapData};

        let project = temp_project();
        let mut state = editor_state(&project);
        let parent = hierarchy::spawn_entity(&mut state.world, "Tilemap", Vec3::ZERO);
        let mut map = TilemapData::new(AssetRef {
            id: uuid::Uuid::new_v4().to_string(),
        });
        map.brush_collision = true;
        state.world.entity_mut(parent).insert(SceneTilemap(map));
        state.select_only(parent);
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        let tile = state
            .world
            .get::<Children>(parent)
            .unwrap()
            .iter()
            .next()
            .copied()
            .unwrap();

        state
            .world
            .get_mut::<SceneTilemap>(parent)
            .unwrap()
            .0
            .brush_collision = false;
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        assert!(state.world.get::<SceneCollider>(tile).is_none());
        assert_eq!(state.world.get::<Children>(parent).unwrap().len(), 1);

        state
            .world
            .get_mut::<SceneTilemap>(parent)
            .unwrap()
            .0
            .brush_collision = true;
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        assert!(state.world.get::<SceneCollider>(tile).is_some());
        assert_eq!(state.world.get::<Children>(parent).unwrap().len(), 1);

        let custom = SceneCollider(engine_scene::ColliderData {
            shape: engine_scene::ColliderShape::Ball { radius: 0.4 },
            body: engine_scene::BodyKind::Dynamic,
        });
        state.world.entity_mut(tile).insert(custom.clone());
        state
            .world
            .get_mut::<SceneTilemap>(parent)
            .unwrap()
            .0
            .brush_collision = false;
        assert!(!state.paint_tile_cell(0, 0, false).unwrap());
        assert_eq!(state.world.get::<SceneCollider>(tile), Some(&custom));
    }

    #[test]
    fn asset_drag_builds_a_component_scene_that_round_trips() {
        use engine_asset::AssetId;
        use engine_ecs::components::{Camera, MeshSource};

        let project = temp_project();
        let mut state = editor_state(&project);
        let asset_path = PathBuf::from("props/crate.gltf");
        let asset_id = AssetId::new();
        state.asset_index.insert(asset_id, asset_path.clone());
        let asset_id_text = asset_id.to_string();

        // This is the same operation used by an Asset Browser drag/drop.
        let crate_entity = state.spawn_asset_entity(&asset_path);
        state
            .world
            .entity_mut(crate_entity)
            .insert(Camera::from(engine_renderer::Camera::new(
                Vec3::new(0.0, 1.0, 3.0),
                Vec3::ZERO,
                16.0 / 9.0,
            )));
        state.mark_dirty();

        let source = state.world.get::<AssetSource>(crate_entity).unwrap();
        assert_eq!(source.path, asset_path.to_string_lossy());
        assert_eq!(source.id.as_deref(), Some(asset_id_text.as_str()));
        assert!(state.world.get::<MeshSource>(crate_entity).is_some());
        assert!(state.world.get::<Camera>(crate_entity).is_some());

        state.save_scene().expect("save authored scene");
        let mut reopened = editor_state(&project);
        reopened.load_scene().expect("reopen authored scene");
        let mut names = reopened.world.query::<&Name>();
        assert!(names.iter(&reopened.world).any(|name| name.0 == "crate"));
        let mut meshes = reopened.world.query::<&MeshSource>();
        assert_eq!(meshes.iter(&reopened.world).count(), 1);
    }

    #[test]
    fn dragging_texture_creates_an_authored_sprite() {
        use engine_asset::AssetId;
        use engine_ecs::components::{Sprite, SpriteSource};

        let project = temp_project();
        let mut state = editor_state(&project);
        let path = PathBuf::from("sprites/hero.png");
        let id = AssetId::new();
        state.asset_index.insert(id, path.clone());

        let entity = state.spawn_asset_entity(&path);
        let source = state
            .world
            .get::<SpriteSource>(entity)
            .expect("sprite source");
        assert_eq!(source.atlas, id.to_string());
        assert!(state.world.get::<Sprite>(entity).is_some());

        state.save_scene().expect("save sprite scene");
        let scene = engine_scene::Scene::load_from_file(&state.project.main_scene_path())
            .expect("reload scene file");
        assert_eq!(
            scene.entities[0].sprite.as_ref().unwrap().atlas.id,
            id.to_string()
        );
    }

    #[test]
    fn terrain_stroke_opens_only_an_authored_asset_inside_the_project() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let path =
            crate::terrain_tools::write_new_terrain(&project.0.assets_dir(), &state.terrain_tools)
                .unwrap();
        let relative = path.strip_prefix(project.0.assets_dir()).unwrap();
        state.assets = crate::scan_assets(&project.0.assets_dir()).unwrap();
        state.asset_index = state
            .importer
            .run(&project.0.assets_dir(), &mut state.assets);
        let entity = state.spawn_asset_entity(relative);
        state.select_only(entity);

        let stroke = state.begin_terrain_stroke().unwrap();
        assert_eq!(stroke.entity, entity);
        assert_eq!(stroke.heightmap.resolution(), 65);

        state
            .world
            .entity_mut(entity)
            .insert(AssetSource::new("../outside.gltf"));
        assert!(state.begin_terrain_stroke().is_err());
    }

    #[test]
    fn scatter_selected_model_creates_one_saved_instanced_foliage_group() {
        use engine_asset::AssetId;

        let project = temp_project();
        let mut state = editor_state(&project);
        let terrain_path =
            crate::terrain_tools::write_new_terrain(&project.0.assets_dir(), &state.terrain_tools)
                .unwrap();
        let terrain_relative = terrain_path.strip_prefix(project.0.assets_dir()).unwrap();
        state
            .asset_index
            .insert(AssetId::new(), terrain_relative.to_path_buf());
        let terrain = state.spawn_asset_entity(terrain_relative);
        state.select_only(terrain);

        let model_relative = PathBuf::from("models/grass.gltf");
        let model_path = project.0.assets_dir().join(&model_relative);
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::copy(&terrain_path, &model_path).unwrap();
        let model_id = AssetId::new();
        state.asset_index.insert(model_id, model_relative.clone());
        state.selected_asset = Some(model_relative);

        assert_eq!(state.scatter_selected_mesh_on_terrain().unwrap(), 256);
        assert!(state.dirty.scene());
        state.save_scene().unwrap();
        let mut reopened = editor_state(&project);
        reopened.load_scene().unwrap();
        let mut foliage = reopened.world.query::<&engine_scene::SceneFoliage>();
        let source = foliage.single(&reopened.world).unwrap();
        assert_eq!(source.0.mesh.id, model_id.to_string());
        assert_eq!(source.0.instances.len(), 256);
        assert!(source.0.instances.iter().all(|pose| {
            pose.translation[0].abs() <= 16.0
                && pose.translation[1] == 0.0
                && pose.translation[2].abs() <= 16.0
        }));
    }

    #[test]
    fn scatter_without_a_selected_model_does_not_mutate_the_scene() {
        let project = temp_project();
        let mut state = editor_state(&project);
        assert!(state.scatter_selected_mesh_on_terrain().is_err());
        assert!(!state.dirty.scene());
        assert!(
            state
                .world
                .query::<&engine_scene::SceneFoliage>()
                .iter(&state.world)
                .next()
                .is_none()
        );
    }

    #[test]
    fn finished_terrain_stroke_is_discarded_after_selection_changes() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let first = state.world.spawn_empty().id();
        let second = state.world.spawn_empty().id();
        state.select_only(first);
        state.terrain_stroke = Some(TerrainStroke {
            entity: first,
            relative_model: PathBuf::from("terrain/Terrain.gltf"),
            heightmap: engine_renderer::Heightmap::new(9, 8.0).unwrap(),
            centers: vec![glam::Vec2::ZERO],
        });
        state.select_only(second);

        state.finish_terrain_stroke();

        assert!(state.terrain_stroke.is_none());
        assert!(state.terrain_sculpt_request.is_none());
    }

    #[test]
    fn adopting_a_project_drops_everything_from_the_previous_one() {
        // The failure this prevents is memorable: the new project
        // rendering the old project's meshes, or an undo reaching into
        // a world that no longer exists.
        let first = temp_project();
        let second = temp_project();
        let mut state = editor_state(&first);

        state.world.spawn(Name::new("left over"));
        state.selected_entity = state.world.spawn(Name::new("selected")).id().into();
        state
            .secondary_selection
            .push(state.world.spawn_empty().id());
        state.assets.push(AssetEntry {
            relative_path: PathBuf::from("old.png"),
            kind: crate::AssetKind::Texture,
            id: None,
        });
        state.selected_asset = Some(PathBuf::from("old.png"));
        state.asset_browser.folders.push(PathBuf::from("old"));
        state.asset_browser.target_dir = PathBuf::from("old");
        state.asset_browser.new_folder = Some("draft".to_owned());
        state.asset_browser.new_folder_request = Some(PathBuf::from("old/draft"));
        state.asset_browser.drop_position = Some((10, 20));
        state.asset_browser.operation_request = Some(crate::AssetOperation::Delete {
            path: PathBuf::from("old.png"),
        });
        state.asset_browser.operation_error = Some("old project error".to_owned());
        state.dirty.mark_scene();
        state.pending_action = Some(PendingAction::Quit);

        state.adopt_project(second.0.clone());

        assert_eq!(state.project.root(), second.0.root());
        assert_eq!(state.scene_path, second.0.main_scene_path());
        // Compared against a fresh `World` rather than zero: bevy_ecs
        // keeps internal entities of its own, and the claim here is
        // "nothing of the old project survived", not "no entities".
        assert_eq!(
            state.world.iter_entities().count(),
            World::new().iter_entities().count(),
            "the old project's entities must be gone",
        );
        assert!(state.selected_entity.is_none());
        assert!(state.secondary_selection.is_empty());
        assert!(
            state.assets.is_empty(),
            "asset list belonged to the old project"
        );
        assert!(state.selected_asset.is_none());
        assert!(state.asset_browser.folders.is_empty());
        assert!(state.asset_browser.target_dir.as_os_str().is_empty());
        assert!(state.asset_browser.new_folder.is_none());
        assert!(state.asset_browser.new_folder_request.is_none());
        assert!(state.asset_browser.drop_position.is_none());
        assert!(state.asset_browser.operation_request.is_none());
        assert!(state.asset_browser.operation_error.is_none());
        assert!(!state.history.can_undo(), "undo must not cross projects");
        assert!(
            !state.dirty.any(),
            "a just-opened project has no unsaved edits"
        );
        assert!(
            state.pending_action.is_none(),
            "a prompt about the old project's changes would be nonsense",
        );
    }

    #[test]
    fn tool_scene_edits_share_the_undo_boundary() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state
            .apply_tool_scene_edit(&editor_tools::ToolRequest::CreateEntity {
                name: "Tool Cube".into(),
            })
            .unwrap();
        assert!(state.history.can_undo());
        assert!(
            state
                .world
                .query::<&Name>()
                .iter(&state.world)
                .any(|name| name.0 == "Tool Cube")
        );
        assert!(state.dirty.any());
        state
            .history
            .undo(&mut state.world, &mut state.selected_entity)
            .expect("history");
        assert!(
            !state
                .world
                .query::<&Name>()
                .iter(&state.world)
                .any(|name| name.0 == "Tool Cube")
        );
    }

    #[test]
    fn ambient_sounds_are_added_from_the_selected_sound_once() {
        let project = temp_project();
        let mut state = editor_state(&project);
        assert!(
            state.add_ambient_sound_from_selection().is_err(),
            "nothing selected"
        );
        let wind = std::path::PathBuf::from("audio/wind.wav");
        state
            .asset_index
            .insert(engine_asset::AssetId::new(), wind.clone());
        state.selected_asset = Some(wind);
        state.add_ambient_sound_from_selection().expect("added");
        assert_eq!(state.environment().ambient_sounds.len(), 1);
        assert!(state.dirty.any());
        assert!(
            state.add_ambient_sound_from_selection().is_err(),
            "no duplicates"
        );

        let image = std::path::PathBuf::from("sprites/tree.png");
        state
            .asset_index
            .insert(engine_asset::AssetId::new(), image.clone());
        state.selected_asset = Some(image);
        assert!(
            state.add_ambient_sound_from_selection().is_err(),
            "images are not sounds"
        );

        state.set_environment(engine_scene::EnvironmentData::default());
        assert!(
            state
                .world
                .get_resource::<engine_scene::SceneEnvironment>()
                .is_none()
        );
    }

    #[test]
    fn holding_space_gives_a_temporary_hand_tool() {
        let project = temp_project();
        let mut state = editor_state(&project);
        assert_eq!(state.active_view_tool(), crate::ViewTool::Select);
        state.temporary_hand = true;
        assert_eq!(state.active_view_tool(), crate::ViewTool::Hand);
        state.temporary_hand = false;
        assert_eq!(
            state.active_view_tool(),
            crate::ViewTool::Select,
            "release returns"
        );
        state.view_tool = crate::ViewTool::Hand;
        assert_eq!(state.active_view_tool(), crate::ViewTool::Hand);
    }

    #[test]
    fn a_locked_selection_shows_no_gizmo() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let entity = hierarchy::spawn_entity(&mut state.world, "Terrain", Vec3::ZERO);
        state.select_only(entity);
        assert!(state.gizmo_origin().is_some());
        state
            .world
            .entity_mut(entity)
            .insert(engine_ecs::components::Lock);
        assert!(state.gizmo_origin().is_none());
    }

    #[test]
    fn frame_scene_fits_every_object_at_once() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let before = *state.scene_camera();
        state.frame_scene();
        assert_eq!(
            *state.scene_camera(),
            before,
            "an empty scene keeps the view"
        );

        let west = hierarchy::spawn_entity(&mut state.world, "West", Vec3::new(-30.0, 0.0, 0.0));
        let east = hierarchy::spawn_entity(&mut state.world, "East", Vec3::new(30.0, 0.0, 10.0));
        let leaf = hierarchy::spawn_entity(&mut state.world, "Leaf", Vec3::new(0.0, 0.0, 0.0));
        state
            .world
            .entity_mut(leaf)
            .insert(engine_ecs::prelude::ChildOf(east));
        state.frame_scene();

        let camera = *state.scene_camera();
        assert!(
            (camera.focus - Vec3::new(0.0, 0.0, 5.0)).length() < 0.5,
            "{}",
            camera.focus
        );
        assert!(
            camera.distance > before.distance * 5.0,
            "{}",
            camera.distance
        );
        let _ = west;
    }

    #[test]
    fn framing_with_nothing_selected_shows_the_whole_scene() {
        let project = temp_project();
        let mut state = editor_state(&project);
        hierarchy::spawn_entity(&mut state.world, "West", Vec3::new(-30.0, 0.0, 0.0));
        hierarchy::spawn_entity(&mut state.world, "East", Vec3::new(30.0, 0.0, 10.0));
        state.clear_selection();

        assert!(state.frame_selection());
        while state.scene_cameras.advance_glide(false, 0.05) {}
        let glided = *state.scene_camera();
        state.frame_scene();
        assert!((glided.focus - state.scene_camera().focus).length() < 1e-2);
        assert!((glided.distance - state.scene_camera().distance).abs() < 1e-2);
    }

    #[test]
    fn frame_selection_fits_the_selected_object_and_its_children() {
        let project = temp_project();
        let mut state = editor_state(&project);
        assert!(!state.frame_selection(), "nothing selected");
        let parent = hierarchy::spawn_entity(&mut state.world, "Parent", Vec3::new(10.0, 0.0, 0.0));
        let child = hierarchy::spawn_entity(&mut state.world, "Child", Vec3::new(6.0, 0.0, 0.0));
        state
            .world
            .entity_mut(child)
            .insert(engine_ecs::prelude::ChildOf(parent));
        state.select_only(parent);

        assert!(state.frame_selection());
        assert_eq!(
            state.scene_camera().focus,
            Vec3::ZERO,
            "framing glides, not jumps"
        );
        while state.scene_cameras.advance_glide(false, 0.05) {}
        let camera = *state.scene_camera();
        assert!(
            (camera.focus - Vec3::new(13.0, 0.0, 0.0)).length() < 1e-3,
            "{}",
            camera.focus
        );
        let wide = camera.distance;
        state.select_only(child);
        assert!(state.frame_selection());
        while state.scene_cameras.advance_glide(false, 0.05) {}
        assert!(
            state.scene_camera().distance < wide,
            "a single object frames closer"
        );
    }

    #[test]
    fn adopting_a_project_records_it_as_recent() {
        let first = temp_project();
        let second = temp_project();
        let mut state = editor_state(&first);
        state.recent_projects.push(first.0.root());

        state.adopt_project(second.0.clone());

        assert_eq!(
            state.recent_projects.paths.len(),
            2,
            "both projects should be listed",
        );
        assert!(
            state.recent_projects.paths[0].ends_with(
                second
                    .0
                    .root()
                    .file_name()
                    .expect("a project directory has a name")
            ),
            "the project just opened is the newest entry",
        );
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
    fn a_stale_configuration_index_clamps_to_one_that_exists() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let count = state.project.configurations().len();
        assert!(count >= 2, "a scaffolded project has Debug and Release");

        let session = EditorSession {
            version: SESSION_VERSION,
            workspace: Workspace::World,
            panels: PanelVisibility::default(),
            layouts: Vec::new(),
            asset_view: crate::AssetView::default(),
            preview_open: true,
            // Someone deleted configurations since this session was written.
            configuration_index: 99,
            overlays: crate::OverlayToggles::default(),
            last_scene: None,
            ..EditorSession::default()
        };
        state.apply_session(session);

        assert_eq!(state.configuration_index, count - 1);
        assert!(
            state.selected_configuration().is_some(),
            "the toolbar always has something to show"
        );
    }

    #[test]
    fn the_selected_configuration_round_trips_through_a_session() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.configuration_index = 1;

        let captured = state.capture_session();
        assert_eq!(captured.configuration_index, 1);

        let mut reopened = editor_state(&project);
        reopened.apply_session(captured);
        assert_eq!(
            reopened.selected_configuration().map(|c| c.name.as_str()),
            Some("Release")
        );
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
            asset_view: crate::AssetView::default(),
            preview_open: true,
            configuration_index: 0,
            overlays: crate::OverlayToggles::default(),
            last_scene: Some(PathBuf::from("scenes/main.ron")),
            ..EditorSession::default()
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
            asset_view: crate::AssetView::default(),
            preview_open: true,
            configuration_index: 0,
            overlays: crate::OverlayToggles::default(),
            last_scene: Some(PathBuf::from("scenes/does_not_exist.ron")),
            ..EditorSession::default()
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
    fn opening_settings_measures_storage_with_the_limits_off() {
        let project = temp_project();
        let mut state = editor_state(&project);
        assert!(
            !state.settings.storage.automatic,
            "automatic cleanup starts off"
        );
        let root = state.project.root().to_path_buf();
        state.storage.opened(&root);
        assert!(
            state.storage.take_save_needed(),
            "the ledger is written after an open"
        );
        crate::menu::MenuCommand::OpenSettings.apply(&mut state);
        assert!(state.settings_open);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.storage.measured.is_none() && std::time::Instant::now() < deadline {
            state.tick_storage();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let measured = state
            .storage
            .measured
            .as_ref()
            .expect("measured in the background");
        assert_eq!(
            measured.len(),
            1,
            "only projects Studio opened are looked at"
        );
        assert!(!state.storage.busy);
    }

    #[test]
    fn grouping_the_selection_is_one_undoable_step() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let a = crate::hierarchy::spawn_entity(&mut state.world, "Rock A", glam::Vec3::ZERO);
        let b = crate::hierarchy::spawn_entity(&mut state.world, "Rock B", glam::Vec3::X);
        state
            .history
            .settle(&mut state.world, state.selected_entity);
        let names = |state: &mut EditorState| {
            let mut names: Vec<String> = crate::hierarchy::roots(&mut state.world)
                .into_iter()
                .map(|entity| crate::hierarchy::display_name(&state.world, entity))
                .collect();
            names.sort();
            names
        };

        state.select_only(a);
        state.select_toggle(b);
        state
            .history
            .begin_frame(&mut state.world, state.selected_entity);
        assert!(state.group_selection());
        let group = state.selected_entity.expect("the group is selected");
        assert_eq!(names(&mut state), ["Group"]);
        assert!(
            state
                .world
                .get::<Name>(group)
                .is_some_and(|name| name.0 == "Group"),
            "the new group is selected, ready to rename"
        );
        assert!(
            state
                .history
                .settle(&mut state.world, state.selected_entity)
        );

        state
            .history
            .undo(&mut state.world, &mut state.selected_entity)
            .expect("history");
        assert_eq!(names(&mut state), ["Rock A", "Rock B"]);

        state
            .history
            .redo(&mut state.world, &mut state.selected_entity)
            .expect("history");
        let group = crate::hierarchy::roots(&mut state.world)[0];
        state.select_only(group);
        assert!(state.ungroup_selection());
        assert_eq!(state.selection_len(), 2, "the freed objects are selected");
        assert_eq!(names(&mut state), ["Rock A", "Rock B"]);
        assert!(
            !state.ungroup_selection(),
            "plain objects cannot be ungrouped"
        );
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
    fn an_edit_put_back_by_hand_leaves_the_scene_clean() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let cube = state.world.spawn(Name::new("Cube")).id();
        state.save_scene().unwrap();

        state.world.get_mut::<Name>(cube).unwrap().0 = "Box".into();
        state.sync_scene_dirty();
        assert!(state.dirty.scene(), "a real change is unsaved");

        state.world.get_mut::<Name>(cube).unwrap().0 = "Cube".into();
        state.sync_scene_dirty();
        assert!(!state.has_unsaved_changes(), "back to the saved scene");
    }

    #[test]
    fn undoing_back_to_the_saved_scene_clears_the_dirty_flag() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.save_scene().unwrap();

        state
            .history
            .begin_frame(&mut state.world, state.selected_entity);
        state.world.spawn(Name::new("Cube"));
        assert!(
            state
                .history
                .settle(&mut state.world, state.selected_entity)
        );
        state.sync_scene_dirty();
        assert!(state.dirty.scene());

        state.undo();
        assert!(!state.dirty.any());
    }

    #[test]
    fn a_marked_scene_that_matches_its_file_is_not_unsaved() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.save_scene().unwrap();
        state.mark_dirty(); // e.g. a toggle switched on and back off

        assert!(!state.has_unsaved_changes());
    }

    #[test]
    fn an_autosave_matching_the_scene_file_is_dropped_not_recovered() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.world.spawn(Name::new("Cube"));
        state.save_scene().unwrap();
        state.load_scene().unwrap();
        state.dirty.mark_scene();
        state.autosave_scene().unwrap();
        assert!(
            !state.autosave_path().exists(),
            "nothing differs, nothing to back up"
        );

        // Even an autosave left by an older Studio is ignored when it
        // matches the file.
        let text = Scene::from_world(&mut state.world).to_ron_string().unwrap();
        std::fs::create_dir_all(state.autosave_path().parent().unwrap()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(state.autosave_path(), text).unwrap();

        assert!(!state.recover_from_autosave().unwrap());
        assert!(!state.dirty.any());
        assert!(!state.recovered_from_autosave);
        assert!(!state.autosave_path().exists());
    }

    #[test]
    fn moving_the_scene_view_is_remembered_but_never_unsaved() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.world.spawn(Name::new("Cube"));
        state.save_scene().unwrap();

        state
            .history
            .begin_frame(&mut state.world, state.selected_entity);
        state.scene_cameras.active_mut(false).zoom(4.0, false, None);
        state.dimension = EditorDimension::Mode2D;
        state.scene_cameras.active_mut(true).zoom(-2.0, true, None);
        assert!(
            !state
                .history
                .settle(&mut state.world, state.selected_entity)
        );
        assert!(
            !state.has_unsaved_changes(),
            "a view change is not a game change"
        );

        let mut reopened = editor_state(&project);
        reopened.apply_session(state.capture_session());
        assert_eq!(reopened.scene_cameras, state.scene_cameras);
        assert_eq!(reopened.dimension, EditorDimension::Mode2D);
    }

    #[test]
    fn discarding_unsaved_edits_removes_the_autosave() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.save_scene().unwrap();
        state.world.spawn(Name::new("Cube"));
        state.dirty.mark_scene();
        state.autosave_scene().unwrap();
        assert!(state.autosave_path().exists());

        state.discard_unsaved_scene();

        assert!(
            !state.autosave_path().exists(),
            "the next launch must not recover it"
        );
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
