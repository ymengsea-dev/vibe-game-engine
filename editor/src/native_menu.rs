//! macOS system menu for the editor. Menu events wake the existing winit loop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use engine::editor::{EditorState, MenuCommand, MenuOverlay, MenuPanel, Workspace};
use engine::platform::WakeHandle;
use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

pub struct NativeMenu {
    _menu: Menu,
    events: Arc<Mutex<Vec<MenuId>>>,
    items: Vec<MenuItem>,
    checks: Vec<CheckMenuItem>,
    recent_menu: Submenu,
    recent_items: Vec<MenuItem>,
    recent_paths: Vec<PathBuf>,
    configuration_menu: Submenu,
    configuration_items: Vec<CheckMenuItem>,
}

fn command_for_id(id: &str, recent: &[PathBuf]) -> Option<MenuCommand> {
    Some(match id {
        "new_project" => MenuCommand::NewProject,
        "open_project" => MenuCommand::OpenProject,
        "scenes" => MenuCommand::OpenScenes,
        "new_scene" => MenuCommand::NewScene,
        "save_scene_as" => MenuCommand::SaveSceneAs,
        "revert_scene" => MenuCommand::RevertScene,
        "save_scene" => MenuCommand::SaveScene,
        "upgrade_rust_sdk" => MenuCommand::UpgradeRustSdk,
        "export" => MenuCommand::Export,
        "settings" => MenuCommand::OpenSettings,
        "quit" => MenuCommand::Quit,
        "undo" => MenuCommand::Undo,
        "redo" => MenuCommand::Redo,
        "import" => MenuCommand::Import,
        "refresh" => MenuCommand::Refresh,
        "create_empty" => MenuCommand::CreateEmpty,
        "create_sprite" => MenuCommand::CreateSprite,
        "create_tilemap" => MenuCommand::CreateTilemap,
        "create_water" => MenuCommand::CreateWater,
        "create_point_light" => MenuCommand::CreatePointLight,
        "create_ui_label" => MenuCommand::CreateUiLabel,
        "create_ui_button" => MenuCommand::CreateUiButton,
        "create_ui_panel" => MenuCommand::CreateUiElement(engine::scene::UiKind::Panel),
        "create_ui_image" => MenuCommand::CreateUiElement(engine::scene::UiKind::Image),
        "create_ui_progress_bar" => {
            MenuCommand::CreateUiElement(engine::scene::UiKind::ProgressBar)
        }
        "create_ui_slider" => MenuCommand::CreateUiElement(engine::scene::UiKind::Slider),
        "create_ui_toggle" => MenuCommand::CreateUiElement(engine::scene::UiKind::Toggle),
        "create_ui_text_input" => MenuCommand::CreateUiElement(engine::scene::UiKind::TextInput),
        "create_ui_layout" => MenuCommand::CreateUiElement(engine::scene::UiKind::Layout),
        "create_camera" => MenuCommand::CreateCamera,
        "create_sound" => MenuCommand::CreateSound,
        "duplicate_selection" => MenuCommand::DuplicateSelection,
        "copy_selection" => MenuCommand::CopySelection,
        "paste_objects" => MenuCommand::Paste,
        "delete_selected" => MenuCommand::DeleteSelected,
        "group_selection" => MenuCommand::GroupSelection,
        "ungroup" => MenuCommand::Ungroup,
        "overlay_colliders" => MenuCommand::ToggleOverlay(MenuOverlay::Colliders),
        "overlay_lights" => MenuCommand::ToggleOverlay(MenuOverlay::Lights),
        "overlay_cameras" => MenuCommand::ToggleOverlay(MenuOverlay::Cameras),
        "overlay_grid" => MenuCommand::ToggleOverlay(MenuOverlay::Grid),
        "overlay_audio" => MenuCommand::ToggleOverlay(MenuOverlay::Audio),
        "overlay_nav" => MenuCommand::ToggleOverlay(MenuOverlay::Nav),
        "overlays_off" => MenuCommand::AllOverlaysOff,
        "panel_hierarchy" => MenuCommand::TogglePanel(MenuPanel::Hierarchy),
        "panel_inspector" => MenuCommand::TogglePanel(MenuPanel::Inspector),
        "panel_assets" => MenuCommand::TogglePanel(MenuPanel::AssetBrowser),
        "panel_preview" => MenuCommand::TogglePanel(MenuPanel::AssetPreview),
        "panel_code" => MenuCommand::TogglePanel(MenuPanel::Code),
        "panel_ai" => MenuCommand::TogglePanel(MenuPanel::Ai),
        "panel_console" => MenuCommand::TogglePanel(MenuPanel::Console),
        "panel_profiler" => MenuCommand::TogglePanel(MenuPanel::Profiler),
        "reset_layouts" => MenuCommand::ResetLayouts,
        "workspace_scene" => MenuCommand::SetWorkspace(Workspace::World),
        "workspace_code" => MenuCommand::SetWorkspace(Workspace::Code),
        "workspace_ai" => MenuCommand::SetWorkspace(Workspace::Ai),
        "workspace_debug" => MenuCommand::SetWorkspace(Workspace::Debug),
        other if other.starts_with("recent_") => {
            MenuCommand::OpenRecent(recent.get(other[7..].parse::<usize>().ok()?)?.clone())
        }
        other if other.starts_with("configuration_") => {
            MenuCommand::SelectConfiguration(other[14..].parse::<usize>().ok()?)
        }
        _ => return None,
    })
}

fn checked(id: &str, state: &EditorState) -> bool {
    match id {
        "overlay_colliders" => state.overlays.all_colliders,
        "overlay_lights" => state.overlays.lights,
        "overlay_cameras" => state.overlays.cameras,
        "overlay_grid" => state.overlays.grid,
        "overlay_audio" => state.overlays.audio,
        "overlay_nav" => state.overlays.nav,
        "panel_hierarchy" => state.panels.hierarchy,
        "panel_inspector" => state.panels.inspector,
        "panel_assets" => state.panels.asset_browser,
        "panel_preview" => state.asset_browser.preview_open,
        "panel_code" => state.panels.code,
        "panel_ai" => state.panels.ai,
        "panel_console" => state.panels.console,
        "panel_profiler" => state.panels.profiler,
        "workspace_scene" => state.workspace == Workspace::World,
        "workspace_code" => state.workspace == Workspace::Code,
        "workspace_ai" => state.workspace == Workspace::Ai,
        "workspace_debug" => state.workspace == Workspace::Debug,
        other if other.starts_with("configuration_") => {
            other[14..].parse::<usize>().ok() == Some(state.configuration_index)
        }
        _ => false,
    }
}

fn enabled(id: &str, state: &EditorState) -> bool {
    match id {
        "undo" => state.history.can_undo(),
        "redo" => state.history.can_redo(),
        "delete_selected" | "duplicate_selection" | "copy_selection" => state.selection_len() > 0,
        "paste_objects" => state.can_paste_objects(),
        "create_sprite" | "create_tilemap" => state.selected_texture_asset().is_some(),
        "upgrade_rust_sdk" => {
            state.project.language() == engine::project::GameplayLanguage::Rust
                && !state.dirty.any()
                && !state.play.is_playing()
                && !state.export_running
        }
        "export" => {
            state.selected_configuration().is_some()
                && !state.export_running
                && !state.play.is_playing()
        }
        _ => true,
    }
}

impl NativeMenu {
    pub fn new(wake: WakeHandle) -> Result<Self, muda::Error> {
        let menu = Menu::new();
        let mut items = Vec::new();
        let mut checks = Vec::new();

        let app = Submenu::new("RustyEngine Studio", true);
        app.append(&PredefinedMenuItem::about(None, None))?;
        app.append(&PredefinedMenuItem::separator())?;
        app.append(&PredefinedMenuItem::hide(None))?;
        app.append(&PredefinedMenuItem::hide_others(None))?;
        app.append(&PredefinedMenuItem::show_all(None))?;
        app.append(&PredefinedMenuItem::separator())?;
        add_item(&app, &mut items, "quit", "Quit RustyEngine Studio")?;
        menu.append(&app)?;

        let file = Submenu::new("File", true);
        add_item(&file, &mut items, "new_project", "New Project…")?;
        add_item(&file, &mut items, "open_project", "Open Project…")?;
        let recent_menu = Submenu::new("Open Recent", true);
        file.append(&recent_menu)?;
        file.append(&PredefinedMenuItem::separator())?;
        add_item(&file, &mut items, "scenes", "Scenes…")?;
        add_item(&file, &mut items, "new_scene", "New Scene…")?;
        add_item(&file, &mut items, "revert_scene", "Revert Scene")?;
        add_item(&file, &mut items, "save_scene", "Save Scene (⌘S)")?;
        add_item(&file, &mut items, "save_scene_as", "Save Scene As…")?;
        add_item(&file, &mut items, "upgrade_rust_sdk", "Upgrade Rust SDK")?;
        file.append(&PredefinedMenuItem::separator())?;
        add_item(&file, &mut items, "export", "Build & Export…")?;
        file.append(&PredefinedMenuItem::separator())?;
        add_item(&file, &mut items, "settings", "Settings… (⌘,)")?;
        menu.append(&file)?;

        let edit = Submenu::new("Edit", true);
        add_item(&edit, &mut items, "undo", "Undo")?;
        add_item(&edit, &mut items, "redo", "Redo")?;
        menu.append(&edit)?;

        let view = Submenu::new("View", true);
        for (id, label) in [
            ("overlay_colliders", "Colliders"),
            ("overlay_lights", "Lights"),
            ("overlay_cameras", "Cameras"),
            ("overlay_grid", "Ground grid"),
            ("overlay_audio", "Audio emitters"),
            ("overlay_nav", "Nav grid"),
        ] {
            add_check(&view, &mut checks, id, label)?;
        }
        view.append(&PredefinedMenuItem::separator())?;
        add_item(&view, &mut items, "overlays_off", "All off")?;
        menu.append(&view)?;

        let gameobject = Submenu::new("GameObject", true);
        add_item(&gameobject, &mut items, "create_empty", "Create Empty")?;
        add_item(
            &gameobject,
            &mut items,
            "create_sprite",
            "Create Sprite from Selected Image",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "create_tilemap",
            "Create Tilemap from Selected Image",
        )?;
        add_item(&gameobject, &mut items, "create_water", "Create Water")?;
        add_item(
            &gameobject,
            &mut items,
            "create_point_light",
            "Create Point Light",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "create_ui_label",
            "Create UI Label",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "create_ui_button",
            "Create UI Button",
        )?;
        for (id, label) in [
            ("create_ui_panel", "Create UI Panel"),
            ("create_ui_image", "Create UI Image"),
            ("create_ui_progress_bar", "Create UI Progress Bar"),
            ("create_ui_slider", "Create UI Slider"),
            ("create_ui_toggle", "Create UI Toggle"),
            ("create_ui_text_input", "Create UI Text Input"),
            ("create_ui_layout", "Create UI Layout"),
        ] {
            add_item(&gameobject, &mut items, id, label)?;
        }
        add_item(&gameobject, &mut items, "create_camera", "Create Camera")?;
        add_item(&gameobject, &mut items, "create_sound", "Create Sound")?;
        add_item(
            &gameobject,
            &mut items,
            "duplicate_selection",
            "Duplicate (⌘D)",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "copy_selection",
            "Copy Objects (⌘C)",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "paste_objects",
            "Paste Objects (⌘V)",
        )?;
        add_item(
            &gameobject,
            &mut items,
            "group_selection",
            "Group Selection (⌘G)",
        )?;
        add_item(&gameobject, &mut items, "ungroup", "Ungroup (⇧⌘G)")?;
        add_item(
            &gameobject,
            &mut items,
            "delete_selected",
            "Delete Selected (⌫)",
        )?;
        menu.append(&gameobject)?;

        let assets = Submenu::new("Assets", true);
        add_item(&assets, &mut items, "import", "Import…")?;
        add_item(&assets, &mut items, "refresh", "Refresh")?;
        menu.append(&assets)?;

        let window = Submenu::new("Window", true);
        for (id, label) in [
            ("panel_hierarchy", "Hierarchy"),
            ("panel_inspector", "Inspector"),
            ("panel_assets", "Asset Browser"),
            ("panel_preview", "Asset Preview"),
            ("panel_code", "Code"),
            ("panel_ai", "AI Assistant"),
            ("panel_console", "Console / Problems / Output"),
            ("panel_profiler", "Profiler"),
        ] {
            add_check(&window, &mut checks, id, label)?;
        }
        let configuration_menu = Submenu::new("Build Configuration", true);
        window.append(&configuration_menu)?;
        add_item(
            &window,
            &mut items,
            "reset_layouts",
            "Reset Layouts to Presets",
        )?;
        window.append(&PredefinedMenuItem::separator())?;
        for (id, label) in [
            ("workspace_scene", "Scene"),
            ("workspace_code", "Code"),
            ("workspace_ai", "AI"),
            ("workspace_debug", "Debug"),
        ] {
            add_check(&window, &mut checks, id, label)?;
        }
        menu.append(&window)?;

        let events = Arc::new(Mutex::new(Vec::new()));
        let queued = Arc::clone(&events);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let mut events = queued
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            events.push(event.id);
            let _ = wake.wake();
        }));
        menu.init_for_nsapp();
        Ok(Self {
            _menu: menu,
            events,
            items,
            checks,
            recent_menu,
            recent_items: Vec::new(),
            recent_paths: Vec::new(),
            configuration_menu,
            configuration_items: Vec::new(),
        })
    }

    pub fn drain(&mut self) -> Vec<MenuCommand> {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ids = std::mem::take(&mut *events);
        ids.iter()
            .filter_map(|id| command_for_id(id.as_ref(), &self.recent_paths))
            .collect()
    }

    pub fn sync(&mut self, state: &EditorState) -> Result<(), muda::Error> {
        for item in &self.items {
            item.set_enabled(enabled(item.id().as_ref(), state));
        }
        for item in &self.checks {
            item.set_checked(checked(item.id().as_ref(), state));
            item.set_enabled(enabled(item.id().as_ref(), state));
        }
        if self.recent_paths != state.recent_projects.paths {
            for item in self.recent_items.drain(..) {
                self.recent_menu.remove(&item)?;
            }
            self.recent_paths = state.recent_projects.paths.clone();
            for (index, path) in self.recent_paths.iter().enumerate() {
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let item = MenuItem::with_id(
                    format!("recent_{index}"),
                    label,
                    path != state.project.root(),
                    None,
                );
                self.recent_menu.append(&item)?;
                self.recent_items.push(item);
            }
        } else {
            for (item, path) in self.recent_items.iter().zip(&self.recent_paths) {
                item.set_enabled(path != state.project.root());
            }
        }
        let names: Vec<_> = state
            .project
            .configurations()
            .iter()
            .map(|config| config.name.clone())
            .collect();
        let old_names: Vec<_> = self
            .configuration_items
            .iter()
            .map(CheckMenuItem::text)
            .collect();
        if names != old_names {
            for item in self.configuration_items.drain(..) {
                self.configuration_menu.remove(&item)?;
            }
            for (index, name) in names.iter().enumerate() {
                let item = CheckMenuItem::with_id(
                    format!("configuration_{index}"),
                    name,
                    true,
                    index == state.configuration_index,
                    None,
                );
                self.configuration_menu.append(&item)?;
                self.configuration_items.push(item);
            }
        } else {
            for item in &self.configuration_items {
                item.set_checked(checked(item.id().as_ref(), state));
            }
        }
        Ok(())
    }
}

fn add_item(
    menu: &Submenu,
    items: &mut Vec<MenuItem>,
    id: &str,
    label: &str,
) -> Result<(), muda::Error> {
    let shortcut = match id {
        "new_project" => Some(Accelerator::new(Modifiers::META, Code::KeyN)),
        "open_project" => Some(Accelerator::new(Modifiers::META, Code::KeyO)),
        "quit" => Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
        _ => None,
    };
    let item = MenuItem::with_id(id, label, true, shortcut);
    menu.append(&item)?;
    items.push(item);
    Ok(())
}

fn add_check(
    menu: &Submenu,
    items: &mut Vec<CheckMenuItem>,
    id: &str,
    label: &str,
) -> Result<(), muda::Error> {
    let item = CheckMenuItem::with_id(id, label, true, false, None);
    menu.append(&item)?;
    items.push(item);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (PathBuf, EditorState) {
        let root = std::env::temp_dir().join(format!(
            "vge_native_menu_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let project = engine::project::Project::create(&root, "Native menu").expect("project");
        let state = EditorState::new(
            project,
            engine::ecs::prelude::World::new(),
            Vec::new(),
            engine::editor::ConsoleLog::new(),
        );
        (root, state)
    }

    #[test]
    fn native_menu_ids_map_only_to_live_commands() {
        assert_eq!(command_for_id("quit", &[]), Some(MenuCommand::Quit));
        assert_eq!(
            command_for_id("create_point_light", &[]),
            Some(MenuCommand::CreatePointLight)
        );
        assert_eq!(
            command_for_id("create_water", &[]),
            Some(MenuCommand::CreateWater)
        );
        assert_eq!(
            command_for_id("create_sprite", &[]),
            Some(MenuCommand::CreateSprite)
        );
        assert_eq!(
            command_for_id("create_tilemap", &[]),
            Some(MenuCommand::CreateTilemap)
        );
        assert_eq!(
            command_for_id("recent_0", &[PathBuf::from("my project")]),
            Some(MenuCommand::OpenRecent(PathBuf::from("my project")))
        );
        assert_eq!(command_for_id("recent_1", &[]), None);
        assert_eq!(command_for_id("placeholder", &[]), None);
    }

    #[test]
    fn native_menu_reflects_edit_selection_export_and_workspace_state() {
        let (root, mut state) = state();
        assert!(!enabled("undo", &state));
        assert!(!enabled("delete_selected", &state));
        assert!(!enabled("create_sprite", &state));
        assert!(enabled("export", &state));
        assert!(checked("workspace_scene", &state));
        state.history.begin_frame(&mut state.world, None);
        let entity = state
            .world
            .spawn(engine::ecs::components::Name::new("Menu entity"))
            .id();
        assert!(state.history.settle(&mut state.world, Some(entity)));
        state.select_only(entity);
        assert!(enabled("undo", &state));
        assert!(enabled("delete_selected", &state));
        state.set_workspace(Workspace::Code);
        assert!(checked("workspace_code", &state));
        assert!(checked("panel_code", &state));
        state.export_running = true;
        assert!(!enabled("export", &state));
        state.export_running = false;
        state.play.start(&mut state.world);
        assert!(!enabled("export", &state));
        let _ = std::fs::remove_dir_all(root);
    }
}
