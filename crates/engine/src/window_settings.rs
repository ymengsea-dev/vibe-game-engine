//! The window a game opens, from the project's game settings (title, size,
//! fullscreen, resizing, icon) rather than only from its code.
//!
//! [`crate::app::run_game`] applies them by itself for an exported game
//! (its `settings.ron`) and for a Rust game Studio runs (`VGE_PROJECT`), so
//! games written before these settings existed follow them too.

use std::path::Path;

use engine_project::WindowSettings;
use engine_project::build::{EXPORT_SETTINGS_FILE, ExportedSettings};

use crate::app::GameConfig;

/// Set by Studio when it runs a Rust game: the project folder.
pub const PROJECT_ENV: &str = "VGE_PROJECT";

/// Applies `title`, `window` and the icon at `icon` to `config`.
fn apply(config: &mut GameConfig, title: &str, window: WindowSettings, icon: Option<&Path>) {
    if !title.trim().is_empty() {
        config.title = title.trim().to_owned();
    }
    config.width = window.width.max(1);
    config.height = window.height.max(1);
    config.fullscreen = window.fullscreen;
    config.resizable = window.resizable;
    if let Some(icon) = icon.and_then(read_icon) {
        config.icon = Some(icon);
    }
}

/// The image at `path` as a window icon, or `None` (logged) if it can't be
/// read.
fn read_icon(path: &Path) -> Option<engine_platform::WindowIcon> {
    let bytes = std::fs::read(path).ok()?;
    match engine_renderer::decode_rgba8(&bytes) {
        Ok(image) => Some(engine_platform::WindowIcon {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        }),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "skipped the window icon");
            None
        }
    }
}

/// Applies an export's settings from `data_dir`. Returns whether it had any.
pub(crate) fn apply_exported(config: &mut GameConfig, data_dir: &Path) -> bool {
    let Some(settings) = std::fs::read_to_string(data_dir.join(EXPORT_SETTINGS_FILE))
        .ok()
        .and_then(|text| ron::from_str::<ExportedSettings>(&text).ok())
    else {
        return false;
    };
    let Some(window) = settings.window else {
        return false;
    };
    let icon = settings.icon.as_deref().map(|name| data_dir.join(name));
    apply(config, &settings.title, window, icon.as_deref());
    true
}

/// Applies the game settings of the project at `root`. Reads `project.ron`
/// only; never migrates or writes it. Returns whether it could.
pub(crate) fn apply_project(config: &mut GameConfig, root: &Path) -> bool {
    let Some(manifest) = std::fs::read_to_string(root.join(engine_project::MANIFEST_FILE))
        .ok()
        .and_then(|text| ron::from_str::<engine_project::ProjectManifest>(&text).ok())
    else {
        return false;
    };
    let game = &manifest.game;
    let icon = game
        .icon
        .as_ref()
        .map(|icon| root.join(engine_project::ASSETS_DIR).join(icon));
    apply(
        config,
        &game.display_name(&manifest.name),
        game.window,
        icon.as_deref(),
    );
    true
}

/// What [`crate::app::run_game`] applies by itself: an export's settings
/// beside the program, else the project Studio names in `VGE_PROJECT`.
/// Nothing while playing inside Studio's Scene View.
pub(crate) fn apply_automatic(config: &mut GameConfig) {
    if std::env::var_os(crate::embed::EMBED_ENV).is_some() {
        return;
    }
    if let Some(dir) = engine_scene::executable_dir()
        && apply_exported(config, &dir)
    {
        return;
    }
    if let Some(root) = std::env::var_os(PROJECT_ENV) {
        apply_project(config, Path::new(&root));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vge-window-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    #[test]
    fn exported_settings_win_over_the_code() {
        let dir = temp("export");
        let settings = ExportedSettings {
            title: "Moon".into(),
            window: Some(WindowSettings {
                width: 800,
                height: 600,
                fullscreen: true,
                resizable: false,
            }),
            ..ExportedSettings::default()
        };
        std::fs::write(
            dir.join(EXPORT_SETTINGS_FILE),
            ron::to_string(&settings).expect("ron"),
        )
        .expect("write");
        let config = GameConfig::new("My Game", 1280, 720).with_exported_settings(&dir);
        assert_eq!(config.title, "Moon");
        assert_eq!((config.width, config.height), (800, 600));
        assert!(config.fullscreen && !config.resizable);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_older_export_keeps_the_codes_window() {
        let dir = temp("old");
        std::fs::write(dir.join(EXPORT_SETTINGS_FILE), "(quality: High)").expect("write");
        let config = GameConfig::new("My Game", 1280, 720).with_exported_settings(&dir);
        assert_eq!(config.title, "My Game");
        assert!(!config.fullscreen);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn project_settings_name_the_window() {
        let dir = temp("project");
        let mut project = engine_project::Project::create_with_language(
            dir.join("game"),
            "Starter",
            engine_project::GameplayLanguage::Lua,
        )
        .expect("project");
        project.manifest_mut().game.name = Some("Star Hop".into());
        project.manifest_mut().game.window.width = 1024;
        project.save_manifest().expect("save");
        let config = GameConfig::new("My Game", 1280, 720).with_project_settings(project.root());
        assert_eq!(config.title, "Star Hop");
        assert_eq!(config.width, 1024);
        let _ = std::fs::remove_dir_all(dir);
    }
}
