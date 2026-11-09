//! Shared per-frame editor state: everything panels read or write,
#![cfg_attr(feature = "dock-shell", allow(dead_code))]
//! bundled into one type so [`crate::EditorShell::run_frame`] doesn't
//! grow an unbounded parameter list as more panels get added (it was
//! already at four before this — hierarchy/inspector's world + selected
//! entity, plus the asset browser's entries + selection — with console
//! and gizmos still to come).

use std::collections::HashMap;
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
    /// Pictures for the game UI drawn over the Scene View, with the import
    /// pass they came from. See `shell::ui_preview_image`.
    pub ui_preview_images:
        std::collections::HashMap<engine_utils::AssetId, (u64, egui::TextureHandle)>,
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
    /// Whether the Scenes window is open.
    pub scenes_window_open: bool,
    /// The dialog naming a new scene, while it is open.
    pub(crate) scene_name_dialog: Option<SceneNameDialog>,
    /// A problem to show in the Scenes window.
    pub(crate) scenes_error: Option<String>,
    /// Scenes open beside the main one, whose objects are in the world too.
    extra_scenes: Vec<ExtraScene>,
    /// The project's scene files, project-relative, and when they were listed.
    scene_files: Vec<PathBuf>,
    /// The folders under `scenes`, listed with `scene_files`.
    scene_folders: Vec<String>,
    scene_files_listed: Option<std::time::Instant>,
    /// The selected prefab copy's status, and when it was worked out.
    prefab_status_cache: Option<(Entity, std::time::Instant, Option<PrefabStatus>)>,
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

/// What the scene naming dialog is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SceneNameKind {
    /// A new empty scene.
    New,
    /// The open scene under another name.
    SaveAs,
    /// A scene's file gets this name.
    Rename(PathBuf),
    /// A new group, with this scene moved into it if there is one.
    NewGroup(Option<PathBuf>),
    /// A group gets this name.
    RenameGroup(String),
}

/// The dialog that names a scene.
#[derive(Debug, Clone)]
pub(crate) struct SceneNameDialog {
    pub(crate) kind: SceneNameKind,
    pub(crate) name: String,
    pub(crate) error: Option<String>,
    pub(crate) focused: bool,
}

impl SceneNameDialog {
    pub(crate) fn new(kind: SceneNameKind) -> Self {
        Self {
            kind,
            name: String::new(),
            error: None,
            focused: false,
        }
    }

    /// The dialog with `name` already typed in.
    pub(crate) fn named(kind: SceneNameKind, name: String) -> Self {
        Self {
            name,
            ..Self::new(kind)
        }
    }
}

/// The group a scene is in: the folder it sits in under `scenes`.
pub fn scene_group_of(scene: &Path) -> Option<String> {
    let parent = scene
        .strip_prefix(engine_project::SCENES_DIR)
        .ok()?
        .parent()?;
    let group = parent.to_string_lossy().replace('\\', "/");
    (!group.is_empty()).then_some(group)
}

/// A scene's name without its group: `scenes/forest/Level 2.ron` is `Level 2`.
pub fn scene_leaf_name(scene: &Path) -> String {
    scene
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every folder under `dir`, as paths relative to `root` with forward slashes.
fn folders_under(root: &Path, dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir()
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_string_lossy().replace('\\', "/"));
            folders_under(root, &path, found);
        }
    }
}

/// Every file under `dir` with one of `extensions`.
fn files_under(dir: &Path, extensions: &[&str], found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            files_under(&path, extensions, found);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            found.push(path);
        }
    }
}

/// Points a scene's rules at a moved scene file. Returns whether any changed.
fn retarget_scene_rules(rules: &mut [engine_scene::RuleData], from: &str, to: &str) -> bool {
    let mut changed = false;
    for rule in rules {
        if let engine_scene::RuleAction::ChangeScene { path }
        | engine_scene::RuleAction::LoadScene { path }
        | engine_scene::RuleAction::UnloadScene { path } = &mut rule.action
            && path == from
        {
            to.clone_into(path);
            changed = true;
        }
    }
    changed
}

/// A scene file's name as people read it: `scenes/forest/Level 2.ron` is
/// `forest/Level 2`.
pub fn scene_display_name(scene: &Path) -> String {
    let inner = scene
        .strip_prefix(engine_project::SCENES_DIR)
        .unwrap_or(scene);
    inner
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/")
}

/// A scene open beside the main one.
#[derive(Debug, Clone)]
struct ExtraScene {
    /// Its project-relative path, with forward slashes.
    path: String,
    /// Its file's settings: lighting, environment and nav grid, with no objects.
    /// Only the main scene's show in Studio, but this one's must not be lost.
    settings: Scene,
}

/// What the Inspector shows for a linked prefab copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefabStatus {
    /// The prefab file, project-relative.
    pub path: String,
    /// Whether the file cannot be read.
    pub missing: bool,
    /// What this copy changes from the prefab.
    pub changes: Vec<String>,
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
            ui_preview_images: std::collections::HashMap::new(),
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
            scenes_window_open: false,
            scene_name_dialog: None,
            scenes_error: None,
            extra_scenes: Vec::new(),
            scene_files: Vec::new(),
            scene_folders: Vec::new(),
            scene_files_listed: None,
            prefab_status_cache: None,
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

    /// The project's scene files, project-relative and sorted. Listed from
    /// disk at most every two seconds; a create or save lists them at once.
    pub fn scene_list(&mut self) -> &[PathBuf] {
        let stale = self
            .scene_files_listed
            .is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(2));
        if stale {
            self.relist_scene_files();
        }
        &self.scene_files
    }

    fn relist_scene_files(&mut self) {
        self.scene_files = self
            .project
            .ordered_scenes()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        self.scene_files_listed = Some(std::time::Instant::now());
        let mut folders = Vec::new();
        folders_under(
            &self.project.scenes_dir(),
            &self.project.scenes_dir(),
            &mut folders,
        );
        folders.sort();
        self.scene_folders = folders;
    }

    /// The groups of scenes, by name. Follows [`Self::scene_list`].
    pub(crate) fn scene_folders(&self) -> &[String] {
        &self.scene_folders
    }

    /// Moves a scene `steps` places up (negative) or down the project's scene
    /// order, saved in `project.ron`.
    ///
    /// # Errors
    ///
    /// A message if the project file can't be written; the order stays.
    pub fn move_scene_in_order(&mut self, scene: &Path, steps: i32) -> Result<(), String> {
        self.relist_scene_files();
        let mut order = self.scene_files.clone();
        let Some(at) = order.iter().position(|listed| listed == scene) else {
            return Ok(());
        };
        let to = at
            .saturating_add_signed(steps as isize)
            .min(order.len() - 1);
        let moved = order.remove(at);
        order.insert(to, moved);
        let previous = std::mem::replace(&mut self.project.manifest_mut().scene_order, order);
        if let Err(err) = self.project.save_manifest() {
            self.project.manifest_mut().scene_order = previous;
            return Err(err.to_string());
        }
        self.relist_scene_files();
        Ok(())
    }

    /// The scenes open beside the main one, project-relative.
    pub fn extra_scenes(&self) -> impl Iterator<Item = &str> {
        self.extra_scenes.iter().map(|extra| extra.path.as_str())
    }

    /// The files of the scenes open beside the main one.
    fn extra_scene_files(&self) -> Vec<String> {
        self.extra_scenes.iter().map(|e| e.path.clone()).collect()
    }

    /// Opens the scene at the project-relative `relative` beside the ones
    /// already open, with its objects in the world. Refuses one already open,
    /// and one sharing object IDs with what is open, as a copy made with Save
    /// Scene As does: the two could not be told apart.
    ///
    /// # Errors
    ///
    /// A message to show the user; nothing has changed.
    pub fn open_scene_beside(&mut self, relative: &Path) -> Result<(), String> {
        let key = relative.to_string_lossy().replace('\\', "/");
        let name = scene_display_name(relative);
        let is_open = self.current_scene().as_deref() == Some(relative)
            || self.extra_scenes.iter().any(|extra| extra.path == key);
        if is_open {
            return Err(format!("{name} is already open."));
        }
        self.sync_scene_dirty();
        let absolute = self.project.resolve(relative).map_err(|e| e.to_string())?;
        let mut scene = Scene::load_from_file(&absolute).map_err(|e| e.to_string())?;
        scene.validate().map_err(|e| e.to_string())?;
        let project = self.project.clone();
        engine_scene::refresh_copies(&mut scene, &mut |path| {
            Self::read_linked_prefab(&project, path)
        });
        scene.assign_missing_object_ids();
        let open = Scene::from_world(&mut self.world);
        let taken: std::collections::HashSet<&String> = open
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.as_ref())
            .collect();
        let shared = scene
            .entities
            .iter()
            .filter_map(|entity| entity.object_id.as_ref())
            .any(|id| taken.contains(id));
        if shared {
            return Err(format!(
                "{name} has the same objects as a scene already open, so it can't open beside it."
            ));
        }
        let mut entities = std::mem::take(&mut scene.entities);
        for entity in &mut entities {
            entity.open_from = Some(key.clone());
        }
        Scene::spawn_entities(&entities, &mut self.world);
        self.extra_scenes.push(ExtraScene {
            path: key,
            settings: scene,
        });
        self.after_scene_set_changed();
        Ok(())
    }

    /// Makes the scene at `relative` the working one: what the Scene View shows,
    /// where new objects go, what Play runs and what the Environment menu edits.
    /// A closed scene opens first. Nothing is saved or lost: unsaved edits stay in
    /// the world and still count as unsaved.
    ///
    /// # Errors
    ///
    /// A message to show the user; the working scene stays.
    pub fn make_working_scene(&mut self, relative: &Path) -> Result<(), String> {
        let Some(previous) = self.current_scene() else {
            return Err("Save this scene in the project first.".to_owned());
        };
        if previous == relative {
            return Ok(());
        }
        let absolute = self.project.resolve(relative).map_err(|e| e.to_string())?;
        let key = relative.to_string_lossy().replace('\\', "/");
        if !self.extra_scenes.iter().any(|extra| extra.path == key) {
            self.open_scene_beside(relative)?;
        }
        let Some(at) = self.extra_scenes.iter().position(|e| e.path == key) else {
            return Ok(());
        };
        self.sync_scene_dirty();
        // A selection in the scene being left would pull straight back to it.
        let selected = self
            .selected_entity
            .filter(|&entity| self.other_scene_of(entity).as_deref() == Some(key.as_str()))
            .and_then(|entity| self.world.get::<engine_scene::SceneObjectId>(entity))
            .map(|id| id.0.clone());
        let previous = previous.to_string_lossy().replace('\\', "/");

        let mut settings = Scene::from_world(&mut self.world);
        // The Scene View tells scenes' objects apart by these.
        settings.assign_missing_object_ids();
        let mut entities = std::mem::take(&mut settings.entities);
        for entity in &mut entities {
            entity.open_from = match entity.open_from.take() {
                Some(file) if file == key => None,
                None => Some(previous.clone()),
                other => other,
            };
        }
        let mut next = std::mem::replace(&mut self.extra_scenes[at].settings, settings);
        self.extra_scenes[at].path = previous;
        next.entities = entities;
        self.scene_path = absolute;
        self.rebuild_world(&next, selected);
        self.relist_scene_files();
        Ok(())
    }

    /// Replaces the world with `scene`, selecting the object with ID `selected`.
    fn rebuild_world(&mut self, scene: &Scene, selected: Option<String>) {
        let mut world = World::new();
        scene.instantiate(&mut world);
        self.world = world;
        self.terrain_stroke = None;
        self.terrain_sculpt_request = None;
        self.tile_paint_active = false;
        self.clear_selection();
        self.selected_entity = selected.and_then(|wanted| {
            let mut query = self.world.query::<(Entity, &engine_scene::SceneObjectId)>();
            query
                .iter(&self.world)
                .find(|(_, id)| id.0 == wanted)
                .map(|(entity, _)| entity)
        });
        self.after_scene_set_changed();
    }

    /// Writes a new empty scene file and makes it the working scene, leaving
    /// the other open scenes open.
    ///
    /// # Errors
    ///
    /// A message to show the user.
    pub fn create_working_scene(&mut self, name: &str) -> Result<(), String> {
        let absolute = self.new_scene_path(name)?;
        if let Some(dir) = absolute.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        Scene::default()
            .save_to_file(&absolute)
            .map_err(|err| err.to_string())?;
        let relative = absolute
            .strip_prefix(self.project.root())
            .map_err(|err| err.to_string())?
            .to_path_buf();
        self.relist_scene_files();
        self.make_working_scene(&relative)
    }

    /// Moves a scene's file inside the `scenes` folder and points everything in
    /// the project at the new path: the project file, every scene's and prefab's
    /// rules, and what is open. Scripts are the author's to change, so they stay
    /// as they are. Returns those that name the old path.
    ///
    /// # Errors
    ///
    /// A message to show the user. The file stays where it was.
    pub fn move_scene_file(&mut self, from: &Path, to: &Path) -> Result<Vec<String>, String> {
        if from == to {
            return Ok(Vec::new());
        }
        let from_key = from.to_string_lossy().replace('\\', "/");
        let to_key = to.to_string_lossy().replace('\\', "/");
        if from_key == engine_project::LOADING_SCENE {
            return Err("The loading screen has to keep its name and place.".to_owned());
        }
        if !engine_scene::valid_scene_path(&to_key) {
            return Err(format!("{to_key} can't be used for a scene."));
        }
        let from_absolute = self.project.resolve(from).map_err(|e| e.to_string())?;
        let to_absolute = self.project.resolve(to).map_err(|e| e.to_string())?;
        if to_absolute.exists() {
            return Err(format!(
                "There is already a scene called {}.",
                scene_display_name(to)
            ));
        }
        self.sync_scene_dirty();
        if let Some(dir) = to_absolute.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::rename(&from_absolute, &to_absolute).map_err(|err| err.to_string())?;

        let saved_manifest = self.project.manifest().clone();
        let manifest = self.project.manifest_mut();
        if manifest.main_scene == from {
            manifest.main_scene = to.to_path_buf();
        }
        for scene in &mut manifest.scene_order {
            if scene == from {
                *scene = to.to_path_buf();
            }
        }
        for configuration in &mut manifest.configurations {
            if configuration.scene.as_deref() == Some(from) {
                configuration.scene = Some(to.to_path_buf());
            }
        }
        if let Err(err) = self.project.save_manifest() {
            *self.project.manifest_mut() = saved_manifest;
            let _ = std::fs::rename(&to_absolute, &from_absolute);
            return Err(err.to_string());
        }

        let mut left_behind = self.retarget_scene_files(&from_key, &to_key);

        if self.scene_path == from_absolute {
            self.scene_path = to_absolute;
        }
        for extra in &mut self.extra_scenes {
            if extra.path == from_key {
                extra.path.clone_from(&to_key);
            }
        }
        let selected = self
            .selected_entity
            .and_then(|entity| self.world.get::<engine_scene::SceneObjectId>(entity))
            .map(|id| id.0.clone());
        let mut scene = Scene::from_world(&mut self.world);
        let mut changed = false;
        for entity in &mut scene.entities {
            if entity.open_from.as_deref() == Some(from_key.as_str()) {
                entity.open_from = Some(to_key.clone());
                changed = true;
            }
            changed |= retarget_scene_rules(&mut entity.rules, &from_key, &to_key);
        }
        if changed {
            self.rebuild_world(&scene, selected);
        }
        self.relist_scene_files();

        let mut scripts = Vec::new();
        files_under(
            &self.project.root().join("assets"),
            &["lua", "rs"],
            &mut scripts,
        );
        files_under(&self.project.root().join("src"), &["rs"], &mut scripts);
        for script in scripts {
            let names_it =
                std::fs::read_to_string(&script).is_ok_and(|text| text.contains(&from_key));
            if names_it && let Ok(relative) = script.strip_prefix(self.project.root()) {
                left_behind.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(left_behind)
    }

    /// Rewrites a moved scene's path in every scene and prefab file. Returns
    /// those that couldn't be written.
    fn retarget_scene_files(&self, from: &str, to: &str) -> Vec<String> {
        let (from, to) = (format!("\"{from}\""), format!("\"{to}\""));
        let mut files = Vec::new();
        files_under(&self.project.scenes_dir(), &["ron"], &mut files);
        files_under(&self.project.root().join("assets"), &["prefab"], &mut files);
        let mut failed = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            if text.contains(&from)
                && write_atomic(&file, text.replace(&from, &to).as_bytes()).is_err()
                && let Ok(relative) = file.strip_prefix(self.project.root())
            {
                failed.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        failed
    }

    /// Gives a scene a new name, in the group it is in.
    ///
    /// # Errors
    ///
    /// A message to show the user.
    pub fn rename_scene(&mut self, scene: &Path, name: &str) -> Result<Vec<String>, String> {
        if name.trim().is_empty() {
            return Err("Type a name for the scene.".to_owned());
        }
        let stem = crate::prefab::sanitize_stem(name);
        let to = scene.with_file_name(format!("{stem}.ron"));
        self.move_scene_file(scene, &to)
    }

    /// Moves a scene into `group`, or out of any group with `None`.
    ///
    /// # Errors
    ///
    /// A message to show the user.
    pub fn move_scene_to_group(
        &mut self,
        scene: &Path,
        group: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let Some(file) = scene.file_name() else {
            return Err("That scene has no file name.".to_owned());
        };
        let mut to = PathBuf::from(engine_project::SCENES_DIR);
        if let Some(group) = group {
            to.push(group);
        }
        to.push(file);
        self.move_scene_file(scene, &to)
    }

    /// Makes an empty group of scenes, a folder under `scenes`. Returns its name.
    ///
    /// # Errors
    ///
    /// A message to show the user.
    pub fn create_scene_group(&mut self, name: &str) -> Result<String, String> {
        if name.trim().is_empty() {
            return Err("Type a name for the group.".to_owned());
        }
        let group = crate::prefab::sanitize_stem(name);
        let folder = self.project.scenes_dir().join(&group);
        if folder.exists() {
            return Err(format!("There is already a group called {group}."));
        }
        std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
        self.relist_scene_files();
        Ok(group)
    }

    /// The scenes directly in `group`.
    fn scenes_in_group(&mut self, group: &str) -> Vec<PathBuf> {
        self.relist_scene_files();
        self.scene_files
            .iter()
            .filter(|scene| scene_group_of(scene).as_deref() == Some(group))
            .cloned()
            .collect()
    }

    /// Gives a group a new name, moving its scenes with it.
    ///
    /// # Errors
    ///
    /// A message to show the user. Scenes already moved stay moved.
    pub fn rename_scene_group(&mut self, group: &str, name: &str) -> Result<Vec<String>, String> {
        let renamed = self.create_scene_group(name)?;
        let mut left_behind = Vec::new();
        for scene in self.scenes_in_group(group) {
            left_behind.extend(self.move_scene_to_group(&scene, Some(&renamed))?);
        }
        let _ = std::fs::remove_dir(self.project.scenes_dir().join(group));
        self.relist_scene_files();
        Ok(left_behind)
    }

    /// Removes a group. Its scenes stay, moved out of it. The folder goes only
    /// if nothing else is in it.
    ///
    /// # Errors
    ///
    /// A message to show the user. Scenes already moved stay moved.
    pub fn delete_scene_group(&mut self, group: &str) -> Result<Vec<String>, String> {
        let mut left_behind = Vec::new();
        for scene in self.scenes_in_group(group) {
            left_behind.extend(self.move_scene_to_group(&scene, None)?);
        }
        let _ = std::fs::remove_dir(self.project.scenes_dir().join(group));
        self.relist_scene_files();
        Ok(left_behind)
    }

    /// Locks or unlocks every object of an open scene; `None` is the working one.
    pub(crate) fn set_scene_locked(&mut self, file: Option<String>, locked: bool) {
        let tops: Vec<Entity> = hierarchy::roots(&mut self.world)
            .into_iter()
            .filter(|&top| self.other_scene_of(top) == file)
            .collect();
        let mut pending = tops;
        while let Some(entity) = pending.pop() {
            pending.extend(hierarchy::children_of(&self.world, entity));
            let Ok(mut object) = self.world.get_entity_mut(entity) else {
                continue;
            };
            if locked {
                object.insert(engine_ecs::components::Lock);
            } else {
                object.remove::<engine_ecs::components::Lock>();
            }
        }
        self.mark_dirty();
    }

    /// The file of the open scene other than the working one that `entity` is in.
    fn other_scene_of(&self, entity: Entity) -> Option<String> {
        if self.world.get_entity(entity).is_err() {
            return None;
        }
        let top = hierarchy::ancestors(&self.world, entity)
            .last()
            .copied()
            .unwrap_or(entity);
        let file = &self.world.get::<engine_scene::SceneFileOwner>(top)?.0;
        self.extra_scenes
            .iter()
            .any(|extra| extra.path == *file)
            .then(|| file.clone())
    }

    /// Selecting an object in another open scene makes that scene the working one.
    pub(crate) fn follow_selection_to_its_scene(&mut self) -> Result<(), String> {
        let Some(file) = self
            .selected_entity
            .and_then(|entity| self.other_scene_of(entity))
        else {
            return Ok(());
        };
        self.make_working_scene(Path::new(&file))
    }

    /// The stable IDs of the top-level objects in the open scenes that aren't the
    /// working one. The Scene View leaves these out.
    pub(crate) fn objects_in_other_scenes(&mut self) -> Vec<String> {
        if self.extra_scenes.is_empty() {
            return Vec::new();
        }
        hierarchy::roots(&mut self.world)
            .into_iter()
            .filter(|&top| self.other_scene_of(top).is_some())
            .filter_map(|top| self.world.get::<engine_scene::SceneObjectId>(top))
            .map(|id| id.0.clone())
            .collect()
    }

    /// Closes a scene opened beside the main one. Its file stays as last saved.
    pub fn close_extra_scene(&mut self, file: &str) {
        self.sync_scene_dirty();
        self.extra_scenes.retain(|extra| extra.path != file);
        let tops = hierarchy::roots(&mut self.world);
        for top in tops {
            let owned = self
                .world
                .get::<engine_scene::SceneFileOwner>(top)
                .is_some_and(|owner| owner.0 == file);
            if owned {
                hierarchy::despawn_entity(&mut self.world, top);
            }
        }
        self.clear_selection();
        self.after_scene_set_changed();
    }

    /// After the open scenes change: the undo history can't cross that. Call
    /// [`Self::sync_scene_dirty`] before changing them. Unsaved edits stay unsaved
    /// until the next save, with nothing to compare them against until then.
    fn after_scene_set_changed(&mut self) {
        self.dragging_handle = None;
        self.history = History::new();
        self.prefab_status_cache = None;
        self.saved_scene = if self.dirty.scene() {
            None
        } else {
            Some(Scene::from_world(&mut self.world))
        };
    }

    /// Moves a top-level object to another open scene's file; `None` is the main
    /// scene. It is written there on the next save.
    pub fn move_object_to_scene(&mut self, entity: Entity, file: Option<String>) {
        let Ok(mut object) = self.world.get_entity_mut(entity) else {
            return;
        };
        match file {
            Some(file) => {
                object.insert(engine_scene::SceneFileOwner(file));
            }
            None => {
                object.remove::<engine_scene::SceneFileOwner>();
            }
        }
        self.mark_dirty();
    }

    /// Makes the loading screen scene, a dark panel with "Loading…" that can
    /// be changed like any Game UI, and opens it.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if it exists already or can't be written.
    pub fn create_loading_screen(&mut self) -> Result<(), SceneError> {
        let relative = Path::new(engine_project::LOADING_SCENE);
        let absolute = self
            .project
            .resolve(relative)
            .map_err(|err| SceneError::Io(err.to_string()))?;
        if absolute.exists() {
            return Err(SceneError::Validation(
                "The project already has a loading screen.".to_owned(),
            ));
        }
        let object = |name: &str, parent, ui| engine_scene::SceneEntity {
            name: Some(name.to_owned()),
            object_id: Some(uuid::Uuid::new_v4().to_string()),
            parent,
            ui: Some(ui),
            ..Default::default()
        };
        let backdrop = engine_scene::UiElementData {
            size: [10_000.0, 10_000.0],
            anchor: engine_scene::UiAnchor::Center,
            color: [0.05, 0.05, 0.08, 1.0],
            ..engine_scene::UiElementData::panel()
        };
        let caption = engine_scene::UiElementData {
            text: "Loading…".to_owned(),
            anchor: engine_scene::UiAnchor::Center,
            ..engine_scene::UiElementData::label()
        };
        Scene {
            entities: vec![
                object("Backdrop", None, backdrop),
                object("Caption", Some(0), caption),
            ],
            ..Default::default()
        }
        .save_to_file(&absolute)?;
        self.open_scene(relative)
    }

    /// The open scene's project-relative path, if it is inside the project.
    pub fn current_scene(&self) -> Option<PathBuf> {
        self.scene_path
            .strip_prefix(self.project.root())
            .ok()
            .map(Path::to_path_buf)
    }

    /// Where a scene named `name` goes: its name made safe for a file, under
    /// the scenes folder. Refuses a blank name and one already in use, so a
    /// scene is never written over.
    fn new_scene_path(&self, name: &str) -> Result<PathBuf, String> {
        if name.trim().is_empty() {
            return Err("Type a name for the scene.".to_owned());
        }
        let stem = crate::prefab::sanitize_stem(name);
        let relative = Path::new(engine_project::SCENES_DIR).join(format!("{stem}.ron"));
        let absolute = self
            .project
            .resolve(&relative)
            .map_err(|err| err.to_string())?;
        if absolute.exists() {
            return Err(format!("There is already a scene called {stem}."));
        }
        Ok(absolute)
    }

    /// Checks `name` can be used for a new scene.
    ///
    /// # Errors
    ///
    /// A message to show the user.
    pub fn check_new_scene_name(&self, name: &str) -> Result<(), String> {
        self.new_scene_path(name).map(|_| ())
    }

    /// Opens the scene at the project-relative `relative`, with a fresh undo
    /// history, so undo can't bring another scene's objects into this one. If
    /// it can't be loaded, the open scene stays as it was.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the path leaves the project or the file can't be loaded.
    pub fn open_scene(&mut self, relative: &Path) -> Result<(), SceneError> {
        let absolute = self
            .project
            .resolve(relative)
            .map_err(|err| SceneError::Io(err.to_string()))?;
        let previous = std::mem::replace(&mut self.scene_path, absolute);
        if let Err(err) = self.load_scene() {
            self.scene_path = previous;
            return Err(err);
        }
        self.dragging_handle = None;
        self.tile_paint_active = false;
        self.history = History::new();
        self.prefab_status_cache = None;
        self.clear_autosave();
        self.relist_scene_files();
        Ok(())
    }

    /// Whether the project has a loading screen scene.
    pub fn has_loading_screen(&self) -> bool {
        self.project
            .resolve(Path::new(engine_project::LOADING_SCENE))
            .is_ok_and(|path| path.is_file())
    }

    /// Writes a new empty scene file and opens it.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the name can't be used or the file can't be written.
    pub fn create_scene(&mut self, name: &str) -> Result<(), SceneError> {
        let absolute = self.new_scene_path(name).map_err(SceneError::Validation)?;
        let dir = absolute.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(dir).map_err(|err| SceneError::Io(err.to_string()))?;
        Scene::default().save_to_file(&absolute)?;
        let relative = absolute
            .strip_prefix(self.project.root())
            .map_err(|err| SceneError::Io(err.to_string()))?
            .to_path_buf();
        self.open_scene(&relative)
    }

    /// Saves the open scene to a new file and carries on in that file. The
    /// old file stays as last saved.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the name can't be used or the file can't be written.
    pub fn save_scene_as(&mut self, name: &str) -> Result<(), SceneError> {
        let absolute = self.new_scene_path(name).map_err(SceneError::Validation)?;
        let previous = std::mem::replace(&mut self.scene_path, absolute);
        if let Err(err) = self.save_scene() {
            self.scene_path = previous;
            return Err(err);
        }
        self.relist_scene_files();
        Ok(())
    }

    /// Makes the game start on the scene at the project-relative `relative`,
    /// saved in `project.ron`.
    ///
    /// # Errors
    ///
    /// A message if the file is missing or the project file can't be written;
    /// the start scene then stays as it was.
    pub fn set_start_scene(&mut self, relative: &Path) -> Result<(), String> {
        let absolute = self
            .project
            .resolve(relative)
            .map_err(|err| err.to_string())?;
        if !absolute.is_file() {
            return Err(format!("{} does not exist", relative.display()));
        }
        let previous = std::mem::replace(
            &mut self.project.manifest_mut().main_scene,
            relative.to_path_buf(),
        );
        if let Err(err) = self.project.save_manifest() {
            self.project.manifest_mut().main_scene = previous;
            return Err(err.to_string());
        }
        Ok(())
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
        self.hierarchy.hidden = session.hierarchy_hidden;
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
            hierarchy_hidden: self.hierarchy.hidden.clone(),
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
        self.stamp_world_overrides();
        let scene = Scene::from_world(&mut self.world);
        let (main, others) = scene.clone().split_by_file(&self.extra_scene_files());
        main.save_to_file(&self.scene_path)?;
        for (extra, entities) in self.extra_scenes.iter().zip(others) {
            let mut file = extra.settings.clone();
            file.entities = entities;
            let absolute = self
                .project
                .resolve(Path::new(&extra.path))
                .map_err(|err| SceneError::Io(err.to_string()))?;
            file.save_to_file(&absolute)?;
        }
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
        // Only the main scene: recovery reads this file back as one scene.
        let scene = Scene::from_world(&mut self.world);
        let (main, _) = scene.split_by_file(&self.extra_scene_files());
        let text = main.to_ron_string()?;
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
        if self
            .world
            .get::<engine_ecs::components::Lock>(parent)
            .is_some()
        {
            return Err("This tilemap is locked. Unlock it to paint.".to_owned());
        }
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
        // Only the folder a game can read prefabs from gets a link.
        let link_path = prefab_path
            .to_str()
            .map(|path| format!("assets/{path}"))
            .filter(|path| engine_scene::valid_prefab_path(path));
        let copies = prefab.copy_entities(link_path.as_deref());
        let spawned = Scene::spawn_entities(&copies, &mut self.world);
        let entity = spawned.first().copied().ok_or(SceneError::Validation(
            "the prefab has no objects".to_owned(),
        ))?;
        self.dirty.mark_scene();
        Ok(entity)
    }

    /// Reads a prefab a copy links to. `None` for a missing or broken file.
    fn read_linked_prefab(project: &Project, path: &str) -> Option<engine_scene::Prefab> {
        if !engine_scene::valid_prefab_path(path) {
            return None;
        }
        let absolute = project.resolve(Path::new(path)).ok()?;
        engine_scene::Prefab::load_from_file(&absolute).ok()
    }

    /// The world as scene data, with each prefab copy's override list current.
    fn scene_with_overrides(&mut self) -> Scene {
        let mut scene = Scene::from_world(&mut self.world);
        let project = self.project.clone();
        engine_scene::stamp_overrides(&mut scene, &mut |path| {
            Self::read_linked_prefab(&project, path)
        });
        scene
    }

    /// Writes each linked object's current override list onto it, so a save
    /// records them and the scene does not read as changed afterwards.
    fn stamp_world_overrides(&mut self) {
        let mut query = self.world.query::<&engine_scene::ScenePrefabLink>();
        if query.iter(&self.world).next().is_none() {
            return;
        }
        let scene = self.scene_with_overrides();
        let links: HashMap<&str, &engine_scene::PrefabLink> = scene
            .entities
            .iter()
            .filter_map(|entity| Some((entity.object_id.as_deref()?, entity.prefab.as_ref()?)))
            .collect();
        let mut query = self.world.query::<(
            &engine_scene::SceneObjectId,
            &mut engine_scene::ScenePrefabLink,
        )>();
        for (id, mut link) in query.iter_mut(&mut self.world) {
            if let Some(current) = links.get(id.0.as_str())
                && link.0 != **current
            {
                link.0 = (*current).clone();
            }
        }
    }

    /// The linked copy `entity` belongs to and what it changes, for the
    /// Inspector. `None` when it is not part of a copy; a copy whose prefab
    /// file cannot be read reports `missing`.
    pub fn prefab_status(&mut self, entity: Entity) -> Option<PrefabStatus> {
        let id = self
            .world
            .get::<engine_scene::SceneObjectId>(entity)?
            .0
            .clone();
        let path = self
            .world
            .get::<engine_scene::ScenePrefabLink>(entity)?
            .0
            .path
            .clone();
        let project = self.project.clone();
        if Self::read_linked_prefab(&project, &path).is_none() {
            return Some(PrefabStatus {
                path,
                missing: true,
                changes: Vec::new(),
            });
        }
        let scene = self.scene_with_overrides();
        let root = engine_scene::copy_root_of(&scene, &id, &mut |path| {
            Self::read_linked_prefab(&project, path)
        })?;
        Some(PrefabStatus {
            path,
            missing: false,
            changes: engine_scene::copy_changes(&scene, root),
        })
    }

    /// [`Self::prefab_status`] for the Inspector, worked out at most twice a
    /// second: it compares against the prefab file and captures the scene.
    pub(crate) fn cached_prefab_status(&mut self, entity: Entity) -> Option<PrefabStatus> {
        if self
            .world
            .get::<engine_scene::ScenePrefabLink>(entity)
            .is_none()
        {
            self.prefab_status_cache = None;
            return None;
        }
        if let Some((cached, at, status)) = &self.prefab_status_cache
            && *cached == entity
            && at.elapsed() < std::time::Duration::from_millis(500)
        {
            return status.clone();
        }
        let status = self.prefab_status(entity);
        self.prefab_status_cache = Some((entity, std::time::Instant::now(), status.clone()));
        status
    }

    /// Replaces the world with `scene`, keeping the selected objects by ID.
    fn reload_world(&mut self, scene: &Scene) {
        let selected = self.selected_entity.and_then(|entity| {
            self.world
                .get::<engine_scene::SceneObjectId>(entity)
                .map(|id| id.0.clone())
        });
        let mut world = World::new();
        scene.instantiate(&mut world);
        self.world = world;
        self.clear_selection();
        if let Some(id) = selected {
            let mut query = self.world.query::<(Entity, &engine_scene::SceneObjectId)>();
            let found = query
                .iter(&self.world)
                .find(|(_, object)| object.0 == id)
                .map(|(entity, _)| entity);
            if let Some(entity) = found {
                self.select_only(entity);
            }
        }
        self.prefab_status_cache = None;
        self.mark_dirty();
    }

    /// Makes the copy `entity` belongs to the new version of its prefab, then
    /// brings the open scene's other copies up to date. The prefab file cannot
    /// be restored by undo.
    ///
    /// # Errors
    ///
    /// [`SceneError`] if the prefab cannot be read, validated or written.
    pub fn apply_prefab_copy(&mut self, entity: Entity) -> Result<(), SceneError> {
        let not_linked = || SceneError::Validation("the object is not a prefab copy".to_owned());
        let id = self
            .world
            .get::<engine_scene::SceneObjectId>(entity)
            .ok_or_else(not_linked)?
            .0
            .clone();
        // Stamped against the old file, so other copies keep what they changed.
        let mut scene = self.scene_with_overrides();
        let project = self.project.clone();
        let mut load = |path: &str| Self::read_linked_prefab(&project, path);
        let root = engine_scene::copy_root_of(&scene, &id, &mut load).ok_or_else(not_linked)?;
        let path = scene
            .entities
            .get(root)
            .and_then(|entity| entity.prefab.as_ref())
            .ok_or_else(not_linked)?
            .path
            .clone();
        let old = load(&path).ok_or_else(not_linked)?;
        let applied = engine_scene::apply_copy(&mut scene, root, &old).ok_or_else(not_linked)?;
        let absolute = self
            .project
            .resolve(Path::new(&path))
            .map_err(|err| SceneError::Io(err.to_string()))?;
        applied.save_atomic(&absolute)?;
        engine_scene::refresh_copies(&mut scene, &mut load);
        self.reload_world(&scene);
        Ok(())
    }

    /// Puts the copy `entity` belongs to back to what its prefab says, keeping
    /// where it is. Undo brings the changes back.
    ///
    /// # Errors
    ///
    /// [`SceneError::Validation`] if the object is not in a copy whose prefab
    /// can be read.
    pub fn revert_prefab_copy(&mut self, entity: Entity) -> Result<(), SceneError> {
        let not_linked = || SceneError::Validation("the object is not a prefab copy".to_owned());
        let id = self
            .world
            .get::<engine_scene::SceneObjectId>(entity)
            .ok_or_else(not_linked)?
            .0
            .clone();
        let mut scene = self.scene_with_overrides();
        let project = self.project.clone();
        let mut load = |path: &str| Self::read_linked_prefab(&project, path);
        let root = engine_scene::copy_root_of(&scene, &id, &mut load).ok_or_else(not_linked)?;
        engine_scene::revert_copy(&mut scene, root);
        engine_scene::refresh_copies(&mut scene, &mut load);
        self.reload_world(&scene);
        Ok(())
    }

    /// Adds the file `path` (relative to the assets root) to the scene, as a
    /// drag from Project Assets does, and selects the new object. It goes at
    /// `position`, keeping a prefab's rotation and scale, or at `parent` as
    /// its child. A prefab that cannot be loaded is reported in Problems.
    pub fn drop_asset(
        &mut self,
        path: &Path,
        position: Option<Vec3>,
        parent: Option<Entity>,
    ) -> Option<Entity> {
        let rows = self.diagnostics.snapshot();
        if rows
            .iter()
            .any(|row| row.code.as_deref() == Some(crate::diagnostics::ASSET_DROP_CODE))
        {
            self.diagnostics.set(
                rows.into_iter()
                    .filter(|row| row.code.as_deref() != Some(crate::diagnostics::ASSET_DROP_CODE))
                    .collect(),
            );
        }
        let is_prefab = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("prefab"));
        let entity = if is_prefab {
            match self.spawn_prefab(path) {
                Ok(entity) => entity,
                Err(err) => {
                    tracing::warn!(error = %err, path = %path.display(), "could not instantiate prefab");
                    self.diagnostics.push(Diagnostic {
                        severity: crate::diagnostics::Severity::Error,
                        code: Some(crate::diagnostics::ASSET_DROP_CODE.to_owned()),
                        message: format!("Could not add {} to the scene: {err}", path.display()),
                        file: Some(Path::new("assets").join(path)),
                        line: None,
                    });
                    return None;
                }
            }
        } else {
            self.spawn_asset_entity(path)
        };
        let parent = parent.filter(|&parent| self.world.get_entity(parent).is_ok());
        if let Some(translation) = position.or(parent.map(|_| Vec3::ZERO)) {
            let mut transform = self
                .world
                .get::<TransformComponent>(entity)
                .map(|transform| transform.0)
                .unwrap_or_default();
            transform.translation = translation;
            self.world
                .entity_mut(entity)
                .insert(TransformComponent::from(transform));
        }
        if let Some(parent) = parent {
            hierarchy::adopt_new_object(&mut self.world, entity, parent);
        }
        self.select_only(entity);
        self.mark_dirty();
        Some(entity)
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
        let mut scene = Scene::load_from_file(Path::new(&self.scene_path))?;
        scene.validate()?;
        let project = self.project.clone();
        let refreshed = engine_scene::refresh_copies(&mut scene, &mut |path| {
            Self::read_linked_prefab(&project, path)
        });
        scene.validate()?;

        let mut world = World::new();
        scene.instantiate(&mut world);
        self.world = world;
        self.extra_scenes.clear();
        self.terrain_stroke = None;
        self.terrain_sculpt_request = None;
        self.clear_selection();
        self.saved_scene = Some(Scene::from_world(&mut self.world));
        self.dirty.clear_scene();
        if refreshed {
            self.dirty.mark_scene();
        }
        self.heal_asset_sources();
        Ok(())
    }

    /// Loads the selected terrain's source data after validating both paths
    /// against the project root. A non-terrain selection is a clean error.
    pub fn begin_terrain_stroke(&self) -> Result<TerrainStroke, String> {
        let entity = self.selected_entity.ok_or("Select a terrain first")?;
        if self
            .world
            .get::<engine_ecs::components::Lock>(entity)
            .is_some()
        {
            return Err("This terrain is locked. Unlock it to sculpt.".to_owned());
        }
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
            if self
                .world
                .get::<engine_ecs::components::Lock>(entity)
                .is_none()
            {
                hierarchy::despawn_entity(&mut self.world, entity);
            }
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

    /// Deep-copies every selected entity next to its original and selects
    /// the copies, the last one primary. Returns whether anything was copied.
    pub fn duplicate_selection(&mut self) -> bool {
        let selection = self.selection();
        hierarchy::duplicate_and_select(
            &mut self.world,
            &mut self.selected_entity,
            &mut self.secondary_selection,
            &selection,
        )
    }

    /// Keeps the selected objects for [`Self::paste_objects`].
    pub fn copy_selection(&mut self) {
        let selection = self.selection();
        if let Some(clipboard) = crate::object_clipboard::copy_objects(&mut self.world, &selection)
        {
            self.hierarchy.clipboard = Some(clipboard);
        }
    }

    /// Forgets the copied objects: their asset ids belong to one project.
    pub fn clear_object_clipboard(&mut self) {
        self.hierarchy.clipboard = None;
    }

    /// Whether [`Self::paste_objects`] has anything to paste.
    pub fn can_paste_objects(&self) -> bool {
        self.hierarchy.clipboard.is_some()
    }

    /// Pastes the copied objects next to the primary selection, or at the
    /// scene root with nothing selected, and selects them. Returns whether
    /// anything was pasted.
    pub fn paste_objects(&mut self) -> bool {
        let Some(clipboard) = self.hierarchy.clipboard.clone() else {
            return false;
        };
        hierarchy::paste_and_select(
            &mut self.world,
            &mut self.selected_entity,
            &mut self.secondary_selection,
            &clipboard,
        )
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

        let custom = SceneCollider(engine_scene::ColliderData::new(
            engine_scene::ColliderShape::Ball { radius: 0.4 },
            engine_scene::BodyKind::Dynamic,
        ));
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

    fn scene_names(state: &mut EditorState) -> Vec<String> {
        state
            .scene_list()
            .iter()
            .map(|scene| scene_display_name(scene))
            .collect()
    }

    #[test]
    fn a_new_scene_gets_its_own_file_and_leaves_the_old_one_alone() {
        let project = temp_project();
        let mut state = editor_state(&project);
        spawn_named(&mut state, "Gone");
        state.save_scene().unwrap();
        let first = state.scene_path.clone();
        let saved = std::fs::read_to_string(&first).unwrap();

        state.create_scene("Level 2").unwrap();

        let named = state.world.query::<&Name>().iter(&state.world).count();
        assert_eq!(named, 0, "the new scene is empty");
        assert!(!state.history.can_undo(), "history reset");
        assert!(!state.dirty.scene(), "it is already on disk");
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/Level 2.ron"))
        );
        assert_eq!(std::fs::read_to_string(&first).unwrap(), saved);
        assert!(scene_names(&mut state).contains(&"Level 2".to_owned()));
    }

    #[test]
    fn a_scene_name_in_use_is_refused_and_nothing_is_overwritten() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_scene("Level 2").unwrap();
        spawn_named(&mut state, "Mine");
        state.save_scene().unwrap();
        let path = state.scene_path.clone();
        let saved = std::fs::read_to_string(&path).unwrap();

        assert!(state.check_new_scene_name("Level 2").is_err());
        assert!(state.check_new_scene_name("   ").is_err());
        assert!(state.check_new_scene_name("../evil").is_ok(), "made safe");
        assert!(state.create_scene("Level 2").is_err());
        assert!(state.save_scene_as("Level 2").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        assert!(!project.0.root().join("evil.ron").exists());
    }

    #[test]
    fn opening_a_scene_swaps_the_world_and_a_broken_one_changes_nothing() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_scene("A").unwrap();
        spawn_named(&mut state, "In A");
        state.save_scene().unwrap();
        state.create_scene("B").unwrap();

        state.open_scene(Path::new("scenes/A.ron")).unwrap();
        let names: Vec<String> = state
            .world
            .query::<&Name>()
            .iter(&state.world)
            .map(|name| name.0.clone())
            .collect();
        assert_eq!(names, ["In A"]);
        assert!(
            !state.history.can_undo(),
            "undo can't reach the other scene"
        );

        std::fs::write(project.0.root().join("scenes/Bad.ron"), "not a scene").unwrap();
        assert!(state.open_scene(Path::new("scenes/Bad.ron")).is_err());
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/A.ron"))
        );
        assert!(state.open_scene(Path::new("../outside.ron")).is_err());
    }

    fn scene_with(state: &mut EditorState, name: &str, object: &str) {
        state.create_scene(name).unwrap();
        spawn_named(state, object);
        state.save_scene().unwrap();
    }

    fn names_in(path: &Path) -> Vec<String> {
        Scene::load_from_file(path)
            .unwrap()
            .entities
            .iter()
            .filter_map(|entity| entity.name.clone())
            .collect()
    }

    #[test]
    fn scenes_open_beside_each_other_save_to_their_own_files() {
        let project = temp_project();
        let mut state = editor_state(&project);
        scene_with(&mut state, "B", "In B");
        scene_with(&mut state, "A", "In A");
        let (a, b) = (
            state.scene_path.clone(),
            project.0.root().join("scenes/B.ron"),
        );

        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        let in_b = named(&mut state, "In B");
        assert!(!state.dirty.scene(), "opening beside changes no file");

        spawn_named(&mut state, "New in A");
        let kid = spawn_named(&mut state, "Kid in B");
        state.world.entity_mut(kid).insert(ChildOf(in_b));
        state.save_scene().unwrap();
        state.sync_scene_dirty();
        assert!(!state.dirty.scene());

        let mut in_file_a = names_in(&a);
        in_file_a.sort();
        assert_eq!(in_file_a, ["In A", "New in A"]);
        let mut in_file_b = names_in(&b);
        in_file_b.sort();
        assert_eq!(in_file_b, ["In B", "Kid in B"]);
    }

    #[test]
    fn moving_an_object_saves_it_to_the_other_scene() {
        let project = temp_project();
        let mut state = editor_state(&project);
        scene_with(&mut state, "B", "In B");
        scene_with(&mut state, "A", "In A");
        let (a, b) = (
            state.scene_path.clone(),
            project.0.root().join("scenes/B.ron"),
        );
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        let in_a = named(&mut state, "In A");

        state.move_object_to_scene(in_a, Some("scenes/B.ron".to_owned()));
        state.save_scene().unwrap();

        assert!(names_in(&a).is_empty());
        assert_eq!(names_in(&b).len(), 2);
    }

    #[test]
    fn closing_a_scene_takes_its_objects_and_leaves_its_file() {
        let project = temp_project();
        let mut state = editor_state(&project);
        scene_with(&mut state, "B", "In B");
        scene_with(&mut state, "A", "In A");
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        state.close_extra_scene("scenes/B.ron");

        let left: Vec<String> = state
            .world
            .query::<&Name>()
            .iter(&state.world)
            .map(|name| name.0.clone())
            .collect();
        assert_eq!(left, ["In A"]);
        assert!(!state.dirty.scene());
        state.save_scene().unwrap();
        assert_eq!(names_in(&project.0.root().join("scenes/B.ron")), ["In B"]);
    }

    #[test]
    fn a_scene_already_open_or_sharing_objects_cant_open_beside() {
        let project = temp_project();
        let mut state = editor_state(&project);
        scene_with(&mut state, "A", "In A");
        state.save_scene_as("Copy of A").unwrap();
        state.open_scene(Path::new("scenes/A.ron")).unwrap();

        assert!(state.open_scene_beside(Path::new("scenes/A.ron")).is_err());
        let shared = state
            .open_scene_beside(Path::new("scenes/Copy of A.ron"))
            .unwrap_err();
        assert!(shared.contains("same objects"));
        assert_eq!(state.extra_scenes().count(), 0);
    }

    /// Two scenes on disk, A with "In A" and B with "In B", with A open alone.
    fn two_scenes(state: &mut EditorState) {
        scene_with(state, "B", "In B");
        scene_with(state, "A", "In A");
    }

    fn scene_file_names(project: &TempProject, file: &str) -> Vec<String> {
        let scene = Scene::load_from_file(&project.0.root().join(file)).unwrap();
        scene
            .entities
            .iter()
            .filter_map(|e| e.name.clone())
            .collect()
    }

    #[test]
    fn changing_the_working_scene_keeps_each_object_in_its_own_file() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        state.make_working_scene(Path::new("scenes/B.ron")).unwrap();
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/B.ron"))
        );
        assert_eq!(state.extra_scenes().collect::<Vec<_>>(), ["scenes/A.ron"]);
        assert!(!state.dirty.scene(), "just switching changes nothing");

        // A new object goes in the working scene.
        spawn_named(&mut state, "New in B");
        state.save_scene().unwrap();
        assert_eq!(scene_file_names(&project, "scenes/A.ron"), ["In A"]);
        let mut b = scene_file_names(&project, "scenes/B.ron");
        b.sort();
        assert_eq!(b, ["In B", "New in B"]);
    }

    #[test]
    fn changing_the_working_scene_keeps_unsaved_edits_unsaved_and_writes_nothing() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        spawn_named(&mut state, "Unsaved");
        state.sync_scene_dirty();
        assert!(state.dirty.scene());

        state.make_working_scene(Path::new("scenes/B.ron")).unwrap();
        state.sync_scene_dirty();
        assert!(state.dirty.scene(), "the edit is still unsaved");
        assert_eq!(scene_file_names(&project, "scenes/A.ron"), ["In A"]);
        let names: Vec<String> = state
            .world
            .query::<&Name>()
            .iter(&state.world)
            .map(|n| n.0.clone())
            .collect();
        assert!(names.iter().any(|name| name == "Unsaved"));

        // Going back and saving puts it in the file it was made in.
        state.make_working_scene(Path::new("scenes/A.ron")).unwrap();
        state.save_scene().unwrap();
        assert!(!state.dirty.scene());
        let mut a = scene_file_names(&project, "scenes/A.ron");
        a.sort();
        assert_eq!(a, ["In A", "Unsaved"]);
        assert_eq!(scene_file_names(&project, "scenes/B.ron"), ["In B"]);
    }

    #[test]
    fn each_scene_keeps_its_own_environment_when_the_working_scene_changes() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        let mut environment = engine_scene::EnvironmentData::default();
        environment
            .ambient_sounds
            .push(engine_scene::AmbientSoundData {
                sound: engine_scene::AssetRef {
                    id: uuid::Uuid::new_v4().to_string(),
                },
                gain: 1.0,
            });
        state.set_environment(environment);
        assert_eq!(state.environment().ambient_sounds.len(), 1);
        state.make_working_scene(Path::new("scenes/B.ron")).unwrap();
        assert_eq!(state.environment().ambient_sounds.len(), 0);
        state.make_working_scene(Path::new("scenes/A.ron")).unwrap();
        assert_eq!(state.environment().ambient_sounds.len(), 1);
    }

    #[test]
    fn a_selection_in_the_scene_being_left_does_not_pull_back_to_it() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        state.selected_entity = Some(named(&mut state, "In A"));
        state.make_working_scene(Path::new("scenes/B.ron")).unwrap();
        state.follow_selection_to_its_scene().unwrap();
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/B.ron"))
        );
        assert_eq!(state.selected_entity, None);
    }

    #[test]
    fn renaming_a_scene_moves_its_file_and_everything_that_names_it() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        state.set_start_scene(Path::new("scenes/B.ron")).unwrap();
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        let in_a = named(&mut state, "In A");
        state
            .world
            .entity_mut(in_a)
            .insert(engine_scene::SceneRules(vec![engine_scene::RuleData::new(
                engine_scene::RuleTrigger::SceneStart,
                engine_scene::RuleAction::ChangeScene {
                    path: "scenes/B.ron".into(),
                },
            )]));
        let scripts = project.0.root().join("assets/scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("go.lua"),
            "engine.change_scene(\"scenes/B.ron\")",
        )
        .unwrap();

        let left = state
            .rename_scene(Path::new("scenes/B.ron"), "Shop")
            .unwrap();
        assert_eq!(left, ["assets/scripts/go.lua"]);
        assert!(project.0.root().join("scenes/Shop.ron").is_file());
        assert!(!project.0.root().join("scenes/B.ron").exists());
        assert_eq!(
            state.extra_scenes().collect::<Vec<_>>(),
            ["scenes/Shop.ron"]
        );
        assert_eq!(
            state.project.manifest().main_scene,
            Path::new("scenes/Shop.ron")
        );
        let in_a = named(&mut state, "In A");
        let rules = state.world.get::<engine_scene::SceneRules>(in_a).unwrap();
        assert_eq!(
            rules.0[0].action,
            engine_scene::RuleAction::ChangeScene {
                path: "scenes/Shop.ron".into()
            }
        );
        state.save_scene().unwrap();
        assert_eq!(scene_file_names(&project, "scenes/Shop.ron"), ["In B"]);
        assert!(
            state
                .rename_scene(Path::new("scenes/Shop.ron"), "A")
                .is_err()
        );
    }

    #[test]
    fn renaming_the_working_scene_with_unsaved_edits_keeps_them() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        spawn_named(&mut state, "Unsaved");
        state.sync_scene_dirty();
        state
            .rename_scene(Path::new("scenes/A.ron"), "Forest")
            .unwrap();
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/Forest.ron"))
        );
        state.sync_scene_dirty();
        assert!(state.dirty.scene());
        state.save_scene().unwrap();
        let mut names = scene_file_names(&project, "scenes/Forest.ron");
        names.sort();
        assert_eq!(names, ["In A", "Unsaved"]);
    }

    #[test]
    fn scene_groups_are_folders_that_scenes_move_in_and_out_of() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        assert_eq!(state.create_scene_group("Levels").unwrap(), "Levels");
        assert!(state.create_scene_group("Levels").is_err());
        assert_eq!(state.scene_folders(), ["Levels"]);

        state
            .move_scene_to_group(Path::new("scenes/B.ron"), Some("Levels"))
            .unwrap();
        assert!(project.0.root().join("scenes/Levels/B.ron").is_file());

        state.rename_scene_group("Levels", "Worlds").unwrap();
        assert!(project.0.root().join("scenes/Worlds/B.ron").is_file());
        assert_eq!(state.scene_folders(), ["Worlds"]);

        state.delete_scene_group("Worlds").unwrap();
        assert!(project.0.root().join("scenes/B.ron").is_file());
        assert!(state.scene_folders().is_empty());
    }

    #[test]
    fn the_loading_screen_keeps_its_name() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_loading_screen().unwrap();
        assert!(
            state
                .rename_scene(Path::new(engine_project::LOADING_SCENE), "Wait")
                .is_err()
        );
    }

    #[test]
    fn locking_a_scene_locks_all_its_objects_and_no_others() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        let in_b = named(&mut state, "In B");
        let kid = spawn_named(&mut state, "Kid");
        state.world.entity_mut(kid).insert(ChildOf(in_b));
        let lock = |state: &EditorState, entity| {
            state
                .world
                .get::<engine_ecs::components::Lock>(entity)
                .is_some()
        };
        state.set_scene_locked(Some("scenes/B.ron".into()), true);
        assert!(lock(&state, in_b) && lock(&state, kid));
        let in_a = named(&mut state, "In A");
        assert!(!lock(&state, in_a));
        state.set_scene_locked(Some("scenes/B.ron".into()), false);
        assert!(!lock(&state, in_b) && !lock(&state, kid));
    }

    #[test]
    fn a_new_scene_leaves_the_others_open_and_their_edits_unsaved() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        spawn_named(&mut state, "Unsaved");
        state.create_working_scene("Shop").unwrap();
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/Shop.ron"))
        );
        assert_eq!(state.extra_scenes().collect::<Vec<_>>(), ["scenes/A.ron"]);
        state.sync_scene_dirty();
        assert!(state.dirty.scene());
    }

    #[test]
    fn only_the_working_scene_shows_and_selecting_elsewhere_follows() {
        let project = temp_project();
        let mut state = editor_state(&project);
        two_scenes(&mut state);
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        assert_eq!(state.objects_in_other_scenes().len(), 1);
        let in_b = state
            .world
            .query::<(Entity, &Name)>()
            .iter(&state.world)
            .find(|(_, name)| name.0 == "In B")
            .map(|(entity, _)| entity)
            .unwrap();
        state.selected_entity = Some(in_b);
        state.follow_selection_to_its_scene().unwrap();
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/B.ron"))
        );
        let selected = state.selected_entity.unwrap();
        assert_eq!(state.world.get::<Name>(selected).unwrap().0, "In B");
    }

    #[test]
    fn opening_a_main_scene_closes_the_others() {
        let project = temp_project();
        let mut state = editor_state(&project);
        scene_with(&mut state, "B", "In B");
        scene_with(&mut state, "A", "In A");
        state.open_scene_beside(Path::new("scenes/B.ron")).unwrap();
        state.open_scene(Path::new("scenes/A.ron")).unwrap();
        assert_eq!(state.extra_scenes().count(), 0);
        let count = state.world.query::<&Name>().iter(&state.world).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn the_scene_order_is_saved_and_a_loading_screen_is_made_once() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_scene("A").unwrap();
        state.create_scene("B").unwrap();
        let before = state.scene_list().to_vec();
        let last = before.last().unwrap().clone();
        state.move_scene_in_order(&last, -1).unwrap();
        let after = state.scene_list().to_vec();
        assert_eq!(after[after.len() - 2], last);
        let reopened = Project::open(project.0.root()).unwrap();
        assert_eq!(
            reopened.ordered_scenes()[after.len() - 2],
            last.to_string_lossy()
        );

        assert!(!state.has_loading_screen());
        state.create_loading_screen().unwrap();
        assert!(state.has_loading_screen());
        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new(engine_project::LOADING_SCENE))
        );
        let saved = Scene::load_from_file(&state.scene_path).unwrap();
        saved.validate().unwrap();
        assert_eq!(saved.entities.len(), 2);
        assert!(state.create_loading_screen().is_err());
    }

    #[test]
    fn save_as_keeps_the_objects_and_moves_to_the_new_file() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_scene("A").unwrap();
        let a = state.scene_path.clone();
        let before = std::fs::read_to_string(&a).unwrap();
        spawn_named(&mut state, "Thing");

        state.save_scene_as("B").unwrap();

        assert_eq!(
            state.current_scene().as_deref(),
            Some(Path::new("scenes/B.ron"))
        );
        assert!(!state.dirty.scene());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), before);
        let saved = Scene::load_from_file(&state.scene_path).unwrap();
        assert_eq!(saved.entities[0].name.as_deref(), Some("Thing"));
    }

    #[test]
    fn the_start_scene_is_saved_in_the_project_file() {
        let project = temp_project();
        let mut state = editor_state(&project);
        state.create_scene("Menu").unwrap();

        state.set_start_scene(Path::new("scenes/Menu.ron")).unwrap();

        let reopened = Project::open(project.0.root()).unwrap();
        assert_eq!(reopened.manifest().main_scene, Path::new("scenes/Menu.ron"));
        assert!(state.set_start_scene(Path::new("scenes/None.ron")).is_err());
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
    fn a_locked_object_is_not_deleted() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let locked = spawn_named(&mut state, "Locked");
        state
            .world
            .entity_mut(locked)
            .insert(engine_ecs::components::Lock);
        state.select_only(locked);
        state.delete_selection();
        assert!(state.world.get_entity(locked).is_ok());
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

    /// Writes `prefabs/Rock.prefab`, with a rotation and scale, and returns
    /// its path relative to the assets root.
    fn write_rock_prefab(state: &EditorState) -> PathBuf {
        let transform = engine_utils::Transform {
            rotation: glam::Quat::from_rotation_y(1.0),
            scale: Vec3::splat(2.0),
            ..Default::default()
        };
        let prefab = engine_scene::Prefab::new(engine_scene::SceneEntity {
            name: Some("Rock".to_owned()),
            transform: Some(transform.into()),
            ..Default::default()
        });
        let assets = state.project.assets_dir();
        std::fs::create_dir_all(assets.join("prefabs")).unwrap();
        prefab
            .save_to_file(&assets.join("prefabs/Rock.prefab"))
            .unwrap();
        PathBuf::from("prefabs/Rock.prefab")
    }

    #[test]
    fn a_dropped_file_lands_at_the_point_and_is_selected() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let at = Vec3::new(1.0, 2.0, 3.0);

        let entity = state
            .drop_asset(Path::new("props/barrel.gltf"), Some(at), None)
            .unwrap();

        assert_eq!(
            state
                .world
                .get::<TransformComponent>(entity)
                .unwrap()
                .0
                .translation,
            at
        );
        assert_eq!(state.selected_entity, Some(entity));
    }

    /// Writes a prefab with a child, and places a linked copy.
    fn place_linked_copy(state: &mut EditorState) -> Entity {
        let dir = state.project.assets_dir().join("prefabs");
        std::fs::create_dir_all(&dir).unwrap();
        let object = |name: &str, parent| engine_scene::SceneEntity {
            name: Some(name.into()),
            object_id: Some(uuid::Uuid::new_v4().to_string()),
            parent,
            ..Default::default()
        };
        engine_scene::Prefab::from_entities(vec![object("Top", None), object("Kid", Some(0))])
            .unwrap()
            .save_to_file(&dir.join("Pair.prefab"))
            .unwrap();
        state
            .drop_asset(Path::new("prefabs/Pair.prefab"), Some(Vec3::X), None)
            .unwrap()
    }

    fn named(state: &mut EditorState, name: &str) -> Entity {
        named_entity(state, name)
    }

    fn named_entity(state: &mut EditorState, name: &str) -> Entity {
        let mut query = state
            .world
            .query::<(Entity, &engine_ecs::components::Name)>();
        query
            .iter(&state.world)
            .find(|(_, candidate)| candidate.0 == name)
            .unwrap()
            .0
    }

    #[test]
    fn a_dropped_prefab_brings_its_children_and_is_linked() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let top = place_linked_copy(&mut state);
        let kid = named_entity(&mut state, "Kid");
        assert_eq!(state.world.get::<ChildOf>(kid).unwrap().parent(), top);
        let status = state.prefab_status(kid).unwrap();
        assert_eq!(status.path, "assets/prefabs/Pair.prefab");
        assert!(status.changes.is_empty() && !status.missing);
    }

    #[test]
    fn apply_writes_the_prefab_and_updates_other_copies() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let first = place_linked_copy(&mut state);
        let second = state
            .drop_asset(Path::new("prefabs/Pair.prefab"), Some(Vec3::Y), None)
            .unwrap();
        state
            .world
            .entity_mut(first)
            .insert(engine_ecs::components::Disabled);
        assert_eq!(state.prefab_status(first).unwrap().changes, ["disabled"]);
        let second_id = state
            .world
            .get::<engine_scene::SceneObjectId>(second)
            .unwrap()
            .0
            .clone();

        state.apply_prefab_copy(first).unwrap();

        let path = state.project.assets_dir().join("prefabs/Pair.prefab");
        let written = engine_scene::Prefab::load_from_file(&path).unwrap();
        assert!(written.entity().disabled);
        let mut query = state.world.query::<(
            &engine_scene::SceneObjectId,
            Option<&engine_ecs::components::Disabled>,
            &TransformComponent,
        )>();
        let (_, disabled, transform) = query
            .iter(&state.world)
            .find(|(id, ..)| id.0 == second_id)
            .unwrap();
        assert!(disabled.is_some(), "the other copy follows");
        assert_eq!(transform.0.translation, Vec3::Y, "and stays where it is");
    }

    #[test]
    fn revert_drops_changes_keeps_position_and_can_be_saved_clean() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let top = place_linked_copy(&mut state);
        state
            .world
            .entity_mut(top)
            .insert(engine_ecs::components::Disabled);
        state.selected_entity = Some(top);

        state.revert_prefab_copy(top).unwrap();

        let top = state.selected_entity.expect("selection survives");
        assert!(
            state
                .world
                .get::<engine_ecs::components::Disabled>(top)
                .is_none()
        );
        assert_eq!(
            state
                .world
                .get::<TransformComponent>(top)
                .unwrap()
                .0
                .translation,
            Vec3::X
        );
    }

    #[test]
    fn a_saved_override_survives_a_prefab_change_on_reload() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let top = place_linked_copy(&mut state);
        let kid = named_entity(&mut state, "Kid");
        state
            .world
            .entity_mut(kid)
            .insert(engine_ecs::components::Disabled);
        state.save_scene().unwrap();
        assert!(!state.dirty.scene());
        state.sync_scene_dirty();
        assert!(!state.dirty.scene(), "stamping does not read as an edit");
        assert_eq!(
            state
                .world
                .get::<engine_scene::ScenePrefabLink>(kid)
                .unwrap()
                .0
                .overrides,
            ["disabled"]
        );
        let _ = top;

        // The prefab gains a tag while the scene is closed.
        let path = state.project.assets_dir().join("prefabs/Pair.prefab");
        let prefab = engine_scene::Prefab::load_from_file(&path).unwrap();
        let mut entities: Vec<_> = prefab.entities().cloned().collect();
        entities[1].tags = vec!["arm".into()];
        engine_scene::Prefab::from_entities(entities)
            .unwrap()
            .save_atomic(&path)
            .unwrap();

        state.load_scene().unwrap();
        let kid = named_entity(&mut state, "Kid");
        assert!(
            state
                .world
                .get::<engine_ecs::components::Disabled>(kid)
                .is_some()
        );
        assert_eq!(
            state.world.get::<engine_scene::SceneTags>(kid).unwrap().0,
            ["arm"]
        );
    }

    #[test]
    fn a_deleted_prefab_file_shows_as_missing_and_the_copy_stays() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let top = place_linked_copy(&mut state);
        let path = state.project.assets_dir().join("prefabs/Pair.prefab");
        std::fs::remove_file(path).unwrap();
        assert!(state.prefab_status(top).unwrap().missing);
        assert!(state.apply_prefab_copy(top).is_err());
        assert!(state.world.get_entity(top).is_ok());
    }

    #[test]
    fn a_dropped_prefab_keeps_its_rotation_and_scale() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let path = write_rock_prefab(&state);
        let at = Vec3::new(4.0, 0.0, 4.0);

        let entity = state.drop_asset(&path, Some(at), None).unwrap();

        let transform = state.world.get::<TransformComponent>(entity).unwrap().0;
        assert_eq!(transform.translation, at);
        assert_eq!(transform.scale, Vec3::splat(2.0));
        assert!(
            transform
                .rotation
                .abs_diff_eq(glam::Quat::from_rotation_y(1.0), 1e-5)
        );
    }

    #[test]
    fn a_file_dropped_on_a_row_becomes_its_child_at_its_position() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let parent = hierarchy::spawn_entity(&mut state.world, "Parent", Vec3::new(5.0, 0.0, 0.0));
        let path = write_rock_prefab(&state);

        let entity = state.drop_asset(&path, None, Some(parent)).unwrap();

        assert_eq!(hierarchy::children_of(&state.world, parent), vec![entity]);
        assert!(
            hierarchy::world_transform(&state.world, entity)
                .translation
                .abs_diff_eq(Vec3::new(5.0, 0.0, 0.0), 1e-5)
        );
    }

    #[test]
    fn a_broken_prefab_is_reported_once_in_problems_and_adds_nothing() {
        let project = temp_project();
        let mut state = editor_state(&project);
        let drop_rows = |state: &EditorState| {
            state
                .diagnostics
                .snapshot()
                .into_iter()
                .filter(|row| row.code.as_deref() == Some(crate::diagnostics::ASSET_DROP_CODE))
                .collect::<Vec<_>>()
        };

        assert!(
            state
                .drop_asset(Path::new("missing.prefab"), None, None)
                .is_none()
        );
        assert_eq!(drop_rows(&state).len(), 1);
        assert_eq!(
            drop_rows(&state)[0].file.as_deref(),
            Some(Path::new("assets/missing.prefab"))
        );
        assert_eq!(state.world.query::<&Name>().iter(&state.world).count(), 0);

        assert!(
            state
                .drop_asset(Path::new("other.prefab"), None, None)
                .is_none()
        );
        let rows = drop_rows(&state);
        assert_eq!(rows.len(), 1, "only the latest failure shows");
        assert!(rows[0].message.contains("other.prefab"));
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
        // Both copies are selected, the last one primary.
        assert_eq!(state.selection_len(), 2);
        assert!(!state.selection().contains(&a));
        assert!(!state.selection().contains(&b));
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
