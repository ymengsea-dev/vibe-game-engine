//! Standalone RustyEngine runtime.
//!
//! Runs an *exported* game folder: a `main.ron` scene plus an
//! `assets.pak` bundle (or a loose `assets/` directory as a fallback).
//! Nothing here links the editor or AI crates — `engine` is depended on
//! with `default-features = false` so the `studio` feature is off
//! (NFR-004).
//!
//! Usage: `player [<export-dir>]` (defaults to the current directory).
//!
//! ## Shape
//!
//! A thin [`Game`] implementation, nothing more. [`run_game`] owns the
//! window, GPU, render pipelines, world and frame loop; [`Player::setup`]
//! loads the bundle, parses the scene, and resolves its asset references
//! into live renderable components through the engine's own
//! [`RuntimeResolver`], the same one a hand-written game uses. Deliberate:
//! the player exercises the same public API a hand-written game uses, so
//! anything missing from it shows up here first.
//!
//! ## Failure stance
//!
//! An export is untrusted input. A missing or corrupt bundle is a clean
//! error and a non-zero exit; a single broken asset reference inside an
//! otherwise-good scene is a warning and a skipped entity, so one bad
//! file never costs you the rest of the level.

use std::path::{Path, PathBuf};

use engine::prelude::{
    AssetLibrary, EXPORT_SCENE_FILE, Game, GameConfig, GameContext, GameError, RuntimeResolver,
    Scene, SceneError, open_export, run_game,
};

/// Opens the export's assets: a packed `assets.pak` if there is one,
/// else a loose `assets/` directory.
///
/// Indexing and resolution both come from the engine
/// ([`AssetLibrary`], [`RuntimeResolver`]) rather than living here. They
/// used to: this file carried its own id index, its own `.meta` parser
/// and its own resolver, all of which the next game would have had to
/// copy.
fn open_assets(export_dir: &Path) -> Option<AssetLibrary> {
    // The packed case is the engine's own `open_export`, the same call a
    // shipped game makes — the player must not have a second way of
    // reading an export, or the two drift and only one gets tested.
    match open_export(export_dir) {
        Some(Ok(content)) => return Some(content.library),
        Some(Err(err)) => {
            tracing::warn!(error = %err, "the export is unreadable; trying loose files");
        }
        None => {}
    }

    let loose = export_dir.join("assets");
    if loose.is_dir() {
        tracing::info!(dir = %loose.display(), "using loose asset directory");
        match AssetLibrary::index(&loose) {
            Ok(library) => return Some(library),
            Err(err) => tracing::warn!(error = %err, "asset directory could not be indexed"),
        }
    }
    None
}

/// Errors that stop the player before a window ever opens.
#[derive(Debug, thiserror::Error)]
enum PlayerError {
    /// No `main.ron` in the export directory or its bundle.
    #[error("no main.ron scene found in {0}")]
    MissingScene(PathBuf),
    /// `main.ron` exists but isn't a usable scene.
    #[error("scene could not be loaded: {0}")]
    Scene(#[from] SceneError),
    /// The scene text wasn't valid UTF-8.
    #[error("main.ron is not valid UTF-8 text")]
    SceneEncoding,
}

/// Reads and parses the export's scene.
fn load_scene(export_dir: &Path) -> Result<Scene, PlayerError> {
    let path = export_dir.join(EXPORT_SCENE_FILE);
    let bytes =
        std::fs::read(&path).map_err(|_| PlayerError::MissingScene(export_dir.to_path_buf()))?;
    let text = String::from_utf8(bytes).map_err(|_| PlayerError::SceneEncoding)?;
    let scene = Scene::from_ron_str(&text)?;
    scene.validate()?;
    Ok(scene)
}

/// The exported game, as a [`Game`].
struct Player {
    scene: Scene,
    assets: Option<AssetLibrary>,
}

impl Game for Player {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        // Adopt the scene's own camera, if it defines one, before
        // anything else looks at the view.
        if let Some(camera) = self
            .scene
            .entities
            .iter()
            .find_map(|entity| entity.camera.as_ref())
        {
            *ctx.camera_mut() = engine::renderer::Camera::from(*camera);
        }

        let report = {
            let parts = ctx.scene_parts();
            match &self.assets {
                Some(library) => {
                    let mut resolver =
                        RuntimeResolver::new(library, parts.gpu, parts.pipeline, parts.assets);
                    self.scene
                        .instantiate_with_resolver(parts.world, &mut resolver)
                }
                None => {
                    tracing::warn!("no asset source; entities will spawn without geometry");
                    self.scene
                        .instantiate_with_resolver(parts.world, &mut engine::scene::NullResolver)
                }
            }
        };

        tracing::info!(
            entities = report.spawned.len(),
            unresolved = report.unresolved,
            "scene instantiated"
        );
        if report.unresolved > 0 {
            tracing::warn!(
                unresolved = report.unresolved,
                "some entities are missing their geometry; the rest of the scene still runs"
            );
        }

        // Physics comes from the same file as the geometry.
        if let Some(library) = &self.assets {
            ctx.spawn_scene_colliders(&self.scene, &report, library);
        }
        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
        if ctx.input().is_key_pressed(engine::prelude::KeyCode::Escape) {
            ctx.request_exit();
        }
    }
}

/// Loads the export and runs it.
fn run(export_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let assets = open_assets(export_dir);
    match &assets {
        Some(library) => tracing::info!(assets = library.len(), "asset index ready"),
        None => tracing::warn!("no assets.pak or assets/ directory found"),
    }

    let scene = load_scene(export_dir)?;

    let config = GameConfig::new("RustyEngine Player", 1280, 720);
    run_game(config, Player { scene, assets })?;
    Ok(())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let export_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    tracing::info!(dir = %export_dir.display(), "loading export");

    if let Err(err) = run(&export_dir) {
        tracing::error!(error = %err, "player exited with an error");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vge-player-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn missing_scene_is_a_clean_error() {
        let dir = temp_dir("empty");
        assert!(matches!(
            load_scene(&dir),
            Err(PlayerError::MissingScene(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_scene_is_an_error_not_a_panic() {
        let dir = temp_dir("bad");
        std::fs::write(dir.join("main.ron"), "this is not RON at all {{{").expect("write");
        assert!(matches!(load_scene(&dir), Err(PlayerError::Scene(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loose_directory_is_used_when_there_is_no_pak() {
        let dir = temp_dir("loose");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");
        std::fs::write(dir.join("assets/thing.gltf"), b"{}").expect("write");
        std::fs::write(
            dir.join("assets/thing.gltf.meta"),
            "(id: \"b8d5d33b-086e-4494-b2bd-0cb556828369\", source_hash: 1)",
        )
        .expect("write");

        let library = open_assets(&dir).expect("a loose dir is a valid source");
        assert_eq!(library.len(), 1);
        assert!(
            library
                .path_for("b8d5d33b-086e-4494-b2bd-0cb556828369")
                .is_some(),
            "a loose asset must resolve to a path, so external references work",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pak_falls_back_to_loose_files_instead_of_failing() {
        let dir = temp_dir("corruptpak");
        std::fs::write(dir.join("assets.pak"), b"not a bundle at all").expect("write");
        std::fs::create_dir_all(dir.join("assets")).expect("assets dir");

        assert!(
            open_assets(&dir).is_some(),
            "an unreadable bundle must fall back, not give up",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_assets_at_all_is_none_not_a_panic() {
        let dir = temp_dir("noassets");
        assert!(open_assets(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
