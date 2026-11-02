//! Commands shared by the in-window and native editor menus.

use std::path::PathBuf;

use glam::Vec3;

use crate::chrome::Workspace;
use crate::dirty::PendingAction;
use crate::{EditorState, OverlayToggles, hierarchy};
use engine_ecs::components::{Name, WaterVolume};

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
    /// Pin an existing Rust project to this Studio build's SDK.
    UpgradeRustSdk,
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
    /// Create a sprite from the selected indexed image asset.
    CreateSprite,
    /// Create a sparse tilemap from the selected image.
    CreateTilemap,
    /// Create a screen-space label.
    CreateUiLabel,
    /// Create a screen-space button.
    CreateUiButton,
    /// Create an editable water surface.
    CreateWater,
    /// Create a positionable point light.
    CreatePointLight,
    /// Delete selected entities.
    DeleteSelected,
    /// Open the Settings window.
    OpenSettings,
    /// Play the game in the Scene View, or stop the preview.
    TogglePreview,
    /// Pause or resume the Scene View preview.
    TogglePause,
    /// Advance a paused preview by one frame.
    StepFrame,
    /// Put the selection into a new group.
    GroupSelection,
    /// Move the selected groups' objects out and remove the groups.
    Ungroup,
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
            Self::UpgradeRustSdk => {
                if state.project.language() == engine_project::GameplayLanguage::Rust
                    && !state.dirty.any()
                    && !state.play.is_playing()
                    && !state.export_running
                {
                    state.sdk_upgrade_request = true;
                }
            }
            Self::Undo => state.undo(),
            Self::Redo => state.redo(),
            Self::Import => state.import_request = true,
            Self::Refresh => state.rescan_request = true,
            Self::CreateEmpty => {
                let entity = hierarchy::spawn_entity(&mut state.world, "Entity", Vec3::ZERO);
                state.select_only(entity);
                state.mark_dirty();
            }
            Self::CreateSprite => {
                if let Some(path) = state.selected_texture_asset().map(PathBuf::from) {
                    let entity = state.spawn_asset_entity(&path);
                    state.select_only(entity);
                    state.mark_dirty();
                }
            }
            Self::CreateTilemap => {
                if let Err(error) = state.create_tilemap_from_selected_image() {
                    state.asset_browser.operation_error = Some(error);
                }
            }
            Self::CreateUiLabel | Self::CreateUiButton => {
                let (name, data) = if matches!(self, Self::CreateUiButton) {
                    ("UI Button", engine_scene::UiElementData::button())
                } else {
                    ("UI Label", engine_scene::UiElementData::label())
                };
                let entity = state
                    .world
                    .spawn((Name::new(name), engine_scene::SceneUi(data)))
                    .id();
                state.select_only(entity);
                state.mark_dirty();
            }
            Self::CreateWater => {
                let entity = state
                    .world
                    .spawn((
                        Name::new("Water"),
                        WaterVolume {
                            surface: Default::default(),
                        },
                    ))
                    .id();
                state.select_only(entity);
                state.mark_dirty();
            }
            Self::CreatePointLight => {
                let entity = hierarchy::spawn_entity(
                    &mut state.world,
                    "Point Light",
                    Vec3::new(0.0, 2.0, 0.0),
                );
                state
                    .world
                    .entity_mut(entity)
                    .insert(engine_scene::ScenePointLight(
                        engine_scene::PointLightData::default(),
                    ));
                state.select_only(entity);
                state.mark_dirty();
            }
            Self::DeleteSelected => {
                if state.selection_len() > 0 {
                    state.delete_selection();
                    state.mark_dirty();
                }
            }
            Self::OpenSettings => {
                state.settings_open = true;
                // Fresh sizes each time the window opens.
                state.storage.refresh();
            }
            Self::TogglePreview => {
                if state.play.is_previewing() {
                    state.play.stop(&mut state.world);
                    state.clear_selection();
                } else if !state.play.is_playing() && !state.export_running {
                    state.game_view.error = None;
                    state.game_view.reset();
                    state.play.start_in_viewport(&mut state.world);
                }
            }
            Self::TogglePause => {
                let control = if state.play.is_paused() {
                    crate::play::GameControl::Resume
                } else {
                    crate::play::GameControl::Pause
                };
                state.play.request(control);
            }
            Self::StepFrame => state.play.request(crate::play::GameControl::Step),
            Self::GroupSelection => {
                if state.group_selection() {
                    state.mark_dirty();
                }
            }
            Self::Ungroup => {
                if state.ungroup_selection() {
                    state.mark_dirty();
                }
            }
            Self::ToggleOverlay(overlay) => {
                let value = match overlay {
                    Overlay::Colliders => &mut state.overlays.all_colliders,
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

#[cfg(test)]
mod water_tests {
    use super::*;
    use engine_ecs::components::{Name, WaterVolume};

    #[test]
    fn create_ui_button_selects_a_saved_clickable_scene_object() {
        let root = std::env::temp_dir().join(format!("vge-ui-menu-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "UI").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );

        MenuCommand::CreateUiButton.apply(&mut state);

        let entity = state.selected_entity.expect("selected UI button");
        let ui = state.world.get::<engine_scene::SceneUi>(entity).unwrap();
        assert_eq!(ui.0.kind, engine_scene::UiKind::Button);
        assert!(state.dirty.scene());
        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert_eq!(saved.entities[0].ui.as_ref().unwrap().text, "Button");
        assert!(saved.entities[0].object_id.is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upgrade_request_only_starts_for_a_clean_idle_rust_project() {
        let lua_root =
            std::env::temp_dir().join(format!("vge-lua-upgrade-{}", uuid::Uuid::new_v4()));
        let lua = engine_project::Project::create(&lua_root, "Lua").unwrap();
        let mut state = EditorState::new(
            lua,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        MenuCommand::UpgradeRustSdk.apply(&mut state);
        assert!(!state.sdk_upgrade_request);

        let rust_root =
            std::env::temp_dir().join(format!("vge-rust-upgrade-{}", uuid::Uuid::new_v4()));
        state.adopt_project(
            engine_project::Project::create_with_language(
                &rust_root,
                "Rust",
                engine_project::GameplayLanguage::Rust,
            )
            .unwrap(),
        );
        state.mark_dirty();
        MenuCommand::UpgradeRustSdk.apply(&mut state);
        assert!(!state.sdk_upgrade_request);
        state.dirty = crate::dirty::DirtyState::default();
        MenuCommand::UpgradeRustSdk.apply(&mut state);
        assert!(state.sdk_upgrade_request);
        let _ = std::fs::remove_dir_all(lua_root);
        let _ = std::fs::remove_dir_all(rust_root);
    }

    #[test]
    fn create_sprite_uses_selected_image_and_saves_it() {
        use engine_ecs::components::{Sprite, SpriteSource};

        let root = std::env::temp_dir().join(format!("vge-sprite-menu-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Sprite").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        let image = PathBuf::from("sprites/hero.png");
        let id = engine_asset::AssetId::new();
        state.asset_index.insert(id, image.clone());
        state.selected_asset = Some(image);

        MenuCommand::CreateSprite.apply(&mut state);

        let entity = state.selected_entity.expect("selected sprite");
        assert!(state.world.get::<Sprite>(entity).is_some());
        assert_eq!(
            state.world.get::<SpriteSource>(entity).unwrap().atlas,
            id.to_string()
        );
        assert!(state.dirty.scene());
        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert_eq!(
            saved.entities[0].sprite.as_ref().unwrap().atlas.id,
            id.to_string()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tile_painting_and_visual_rule_edits_are_undoable() {
        use engine_ecs::prelude::Children;

        let root = std::env::temp_dir().join(format!("vge-tile-undo-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Tile Undo").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        let image = PathBuf::from("sprites/tiles.png");
        state
            .asset_index
            .insert(engine_asset::AssetId::new(), image.clone());
        state.selected_asset = Some(image);
        MenuCommand::CreateTilemap.apply(&mut state);
        let children = |state: &EditorState| {
            state
                .world
                .iter_entities()
                .find(|entity| entity.get::<engine_scene::SceneTilemap>().is_some())
                .and_then(|map| map.get::<Children>().map(|children| children.len()))
                .unwrap_or(0)
        };

        // One editor frame: snapshot, edit, settle — as the shell drives it.
        state
            .history
            .begin_frame(&mut state.world, state.selected_entity);
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        assert!(state.paint_tile_cell(1, 0, false).unwrap());
        assert!(
            state
                .history
                .settle(&mut state.world, state.selected_entity)
        );
        assert_eq!(children(&state), 2);

        let map = state.selected_entity.unwrap();
        state
            .history
            .begin_frame(&mut state.world, state.selected_entity);
        state
            .world
            .entity_mut(map)
            .insert(engine_scene::SceneRules(vec![engine_scene::RuleData::new(
                engine_scene::RuleTrigger::Named("coin".into()),
                engine_scene::RuleAction::AddToVariable {
                    name: "coins".into(),
                    amount: 1.0,
                },
            )]));
        assert!(
            state
                .history
                .settle(&mut state.world, state.selected_entity)
        );

        assert!(
            state
                .history
                .undo(&mut state.world, &mut state.selected_entity)
                .expect("history")
                .is_some()
        );
        let rules = |state: &EditorState| {
            state
                .world
                .iter_entities()
                .filter_map(|entity| entity.get::<engine_scene::SceneRules>().map(|r| r.0.len()))
                .sum::<usize>()
        };
        assert_eq!(rules(&state), 0, "rule edit undone");
        assert_eq!(children(&state), 2, "tiles kept");
        assert!(
            state
                .history
                .undo(&mut state.world, &mut state.selected_entity)
                .expect("history")
                .is_some()
        );
        assert_eq!(children(&state), 0, "painting undone");
        assert!(
            state
                .history
                .redo(&mut state.world, &mut state.selected_entity)
                .expect("history")
                .is_some()
        );
        assert!(
            state
                .history
                .redo(&mut state.world, &mut state.selected_entity)
                .expect("history")
                .is_some()
        );
        assert_eq!((children(&state), rules(&state)), (2, 1), "both redone");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tilemap_paint_replace_erase_and_reopen_keep_one_cell() {
        use engine_ecs::prelude::Children;

        let root = std::env::temp_dir().join(format!("vge-tilemap-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Tilemap").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        let image = PathBuf::from("sprites/tiles.png");
        let id = engine_asset::AssetId::new();
        state.asset_index.insert(id, image.clone());
        state.selected_asset = Some(image);
        MenuCommand::CreateTilemap.apply(&mut state);

        let parent = state.selected_entity.unwrap();
        assert_eq!(
            state
                .world
                .get::<engine_scene::SceneTilemap>(parent)
                .unwrap()
                .0
                .atlas
                .id,
            id.to_string()
        );
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        assert!(!state.paint_tile_cell(0, 0, false).unwrap());
        state
            .world
            .get_mut::<engine_scene::SceneTilemap>(parent)
            .unwrap()
            .0
            .brush_column = 1;
        assert!(state.paint_tile_cell(0, 0, false).unwrap());
        assert!(state.paint_tile_cell(-1, 2, false).unwrap());
        assert!(state.paint_tile_cell(0, 0, true).unwrap());
        assert_eq!(state.world.get::<Children>(parent).unwrap().len(), 1);
        assert!(state.paint_tile_cell(10_001, 0, false).is_err());
        assert_eq!(state.world.get::<Children>(parent).unwrap().len(), 1);

        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert_eq!(saved.entities.len(), 2);
        assert!(saved.entities.iter().any(|entity| {
            entity
                .tilemap
                .as_ref()
                .is_some_and(|map| map.atlas.id == id.to_string())
        }));
        assert!(saved.entities.iter().any(|entity| {
            entity
                .sprite
                .as_ref()
                .is_some_and(|sprite| sprite.region == "grid:4,4:1,0")
        }));
        let mut restored = engine_ecs::prelude::World::new();
        saved.instantiate(&mut restored);
        assert_eq!(
            restored
                .query::<&engine_scene::SceneTilemap>()
                .iter(&restored)
                .count(),
            1
        );
        assert_eq!(
            restored
                .query::<&engine_ecs::components::SpriteSource>()
                .iter(&restored)
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_sprite_without_an_indexed_image_does_nothing() {
        let root = std::env::temp_dir().join(format!("vge-sprite-menu-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Sprite").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        state.asset_index.insert(
            engine_asset::AssetId::new(),
            PathBuf::from("props/rock.glb"),
        );
        for selected in [None, Some("sprites/missing.png"), Some("props/rock.glb")] {
            state.selected_asset = selected.map(PathBuf::from);
            MenuCommand::CreateSprite.apply(&mut state);
            assert!(state.selected_entity.is_none());
            assert!(!state.dirty.scene());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_water_selects_a_saved_scene_object() {
        let root = std::env::temp_dir().join(format!("vge-water-menu-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Water").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );

        MenuCommand::CreateWater.apply(&mut state);

        let entity = state.selected_entity.expect("selected water");
        assert_eq!(state.world.get::<Name>(entity).unwrap().0, "Water");
        assert!(state.world.get::<WaterVolume>(entity).is_some());
        assert!(state.dirty.scene());
        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert!(saved.entities[0].water.is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn create_point_light_selects_a_saved_positionable_light() {
        let root = std::env::temp_dir().join(format!("vge-light-menu-{}", uuid::Uuid::new_v4()));
        let project = engine_project::Project::create(&root, "Light").unwrap();
        let mut state = EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );

        MenuCommand::CreatePointLight.apply(&mut state);

        let entity = state.selected_entity.expect("selected light");
        assert!(
            state
                .world
                .get::<engine_ecs::components::Transform>(entity)
                .is_some()
        );
        assert!(
            state
                .world
                .get::<engine_scene::ScenePointLight>(entity)
                .is_some()
        );
        state.save_scene().unwrap();
        let saved = engine_scene::Scene::load_from_file(&state.scene_path).unwrap();
        assert!(saved.entities[0].point_light.is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
