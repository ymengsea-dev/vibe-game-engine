//! Commands shared by the in-window and native editor menus.

use std::path::PathBuf;

use glam::Vec3;

use crate::chrome::Workspace;
use crate::dirty::PendingAction;
use crate::{EditorState, OverlayToggles, hierarchy};

/// An implemented menu action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuCommand {
    /// Show the new-project dialog.
    NewProject,
    /// Choose an existing project.
    OpenProject,
    /// Open a recently used project.
    OpenRecent(PathBuf),
    /// Create a scene after the unsaved-work guard.
    NewScene,
    /// Reload the scene after the unsaved-work guard.
    RevertScene,
    /// Save the scene.
    SaveScene,
    /// Choose an export destination.
    Export,
    /// Quit after the unsaved-work guard.
    Quit,
    /// Undo the last scene edit.
    Undo,
    /// Redo the last scene edit.
    Redo,
    /// Import assets.
    Import,
    /// Refresh the asset index.
    Refresh,
    /// Create an empty entity.
    CreateEmpty,
    /// Delete selected entities.
    DeleteSelected,
    /// Toggle one scene overlay.
    ToggleOverlay(Overlay),
    /// Disable every scene overlay.
    AllOverlaysOff,
    /// Toggle a panel.
    TogglePanel(Panel),
    /// Select a build configuration by index.
    SelectConfiguration(usize),
    /// Restore the built-in workspace layouts.
    ResetLayouts,
    /// Switch workspace.
    SetWorkspace(Workspace),
}

/// Scene overlay selected by a menu action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// Collider geometry.
    Colliders,
    /// Light markers.
    Lights,
    /// Camera frusta.
    Cameras,
    /// Ground grid.
    Grid,
    /// Audio emitters.
    Audio,
    /// Navigation grid.
    Nav,
}

/// Editor panel selected by a menu action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// Scene hierarchy.
    Hierarchy,
    /// Entity inspector.
    Inspector,
    /// Project assets.
    AssetBrowser,
    /// Selected asset preview.
    AssetPreview,
    /// Code editor.
    Code,
    /// AI assistant.
    Ai,
    /// Console and output.
    Console,
    /// Frame profiler.
    Profiler,
}

pub(crate) enum MenuEffect {
    None,
    NewProjectDialog,
    OpenProjectDialog,
    ExportDialog,
}

impl MenuCommand {
    pub(crate) fn apply(self, state: &mut EditorState) -> MenuEffect {
        match self {
            Self::NewProject => return MenuEffect::NewProjectDialog,
            Self::OpenProject => return MenuEffect::OpenProjectDialog,
            Self::Export => return MenuEffect::ExportDialog,
            Self::OpenRecent(path) => {
                crate::shell::request_guarded(state, PendingAction::OpenProject(path))
            }
            Self::NewScene => crate::shell::request_guarded(state, PendingAction::NewScene),
            Self::RevertScene => crate::shell::request_guarded(state, PendingAction::RevertScene),
            Self::Quit => crate::shell::request_guarded(state, PendingAction::Quit),
            Self::SaveScene => match state.save_scene() {
                Ok(()) => tracing::info!(path = %state.scene_path.display(), "scene saved"),
                Err(err) => tracing::warn!(error = %err, "scene save failed"),
            },
            Self::Undo => {
                state
                    .history
                    .undo(&mut state.world, &mut state.selected_entity);
                state.secondary_selection.clear();
                state.dragging_axis = None;
            }
            Self::Redo => {
                state
                    .history
                    .redo(&mut state.world, &mut state.selected_entity);
                state.secondary_selection.clear();
                state.dragging_axis = None;
            }
            Self::Import => state.import_request = true,
            Self::Refresh => state.rescan_request = true,
            Self::CreateEmpty => {
                let entity = hierarchy::spawn_entity(&mut state.world, "Entity", Vec3::ZERO);
                state.select_only(entity);
                state.mark_dirty();
            }
            Self::DeleteSelected => {
                if state.selection_len() > 0 {
                    state.delete_selection();
                    state.mark_dirty();
                }
            }
            Self::ToggleOverlay(overlay) => {
                let value = match overlay {
                    Overlay::Colliders => &mut state.overlays.colliders,
                    Overlay::Lights => &mut state.overlays.lights,
                    Overlay::Cameras => &mut state.overlays.cameras,
                    Overlay::Grid => &mut state.overlays.grid,
                    Overlay::Audio => &mut state.overlays.audio,
                    Overlay::Nav => &mut state.overlays.nav,
                };
                *value = !*value;
            }
            Self::AllOverlaysOff => state.overlays = OverlayToggles::default(),
            Self::TogglePanel(panel) => {
                let value = match panel {
                    Panel::Hierarchy => &mut state.panels.hierarchy,
                    Panel::Inspector => &mut state.panels.inspector,
                    Panel::AssetBrowser => &mut state.panels.asset_browser,
                    Panel::AssetPreview => &mut state.asset_browser.preview_open,
                    Panel::Code => &mut state.panels.code,
                    Panel::Ai => &mut state.panels.ai,
                    Panel::Console => &mut state.panels.console,
                    Panel::Profiler => &mut state.panels.profiler,
                };
                *value = !*value;
            }
            Self::SelectConfiguration(index) => {
                if state.project.configuration(index).is_some() {
                    state.configuration_index = index;
                }
            }
            Self::ResetLayouts => state.reset_workspace_layouts(),
            Self::SetWorkspace(workspace) => state.set_workspace(workspace),
        }
        MenuEffect::None
    }
}
