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
//! ## Status
//!
//! Loads the bundle + scene, instantiates the world, and opens a window
//! running a render loop that clears to the scene's background. Drawing
//! the scene's meshes/sprites reuses the same `engine_ecs` extract path
//! the `game` binary uses — wiring that render pipeline in here is the
//! next slice; today the player proves the *runtime crate graph* and the
//! bundle/scene load path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::asset::Bundle;
use engine::ecs::prelude::World;
use engine::platform::{PlatformEvent, PlatformHandler, Window, WindowConfig, run_windowed};
use engine::renderer::GpuContext;
use engine::scene::Scene;

/// Where a game's assets come from at runtime: a packed `.pak` or a
/// loose directory.
enum AssetSource {
    /// A parsed `assets.pak`.
    Bundle(Bundle),
    /// A `<dir>/assets/` tree.
    Loose(PathBuf),
}

impl AssetSource {
    /// Resolves the asset source for an export directory: prefer
    /// `assets.pak`, fall back to a loose `assets/` dir, else `None`.
    fn resolve(export_dir: &Path) -> Option<AssetSource> {
        let pak = export_dir.join("assets.pak");
        if pak.is_file() {
            match Bundle::open(&pak) {
                Ok(bundle) => {
                    tracing::info!(files = bundle.len(), "opened asset bundle");
                    return Some(AssetSource::Bundle(bundle));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "assets.pak is unreadable; trying loose files")
                }
            }
        }
        let loose = export_dir.join("assets");
        if loose.is_dir() {
            tracing::info!(dir = %loose.display(), "using loose asset directory");
            return Some(AssetSource::Loose(loose));
        }
        None
    }

    /// Reads one asset by its bundle-relative path.
    fn read(&self, relative: &str) -> Option<Vec<u8>> {
        match self {
            AssetSource::Bundle(bundle) => bundle.get(relative).map(<[u8]>::to_vec),
            AssetSource::Loose(dir) => std::fs::read(dir.join(relative)).ok(),
        }
    }

    /// How many assets are reachable (bundle entry count, or a directory
    /// walk for the loose case).
    fn count(&self) -> usize {
        match self {
            AssetSource::Bundle(bundle) => bundle.len(),
            AssetSource::Loose(dir) => walk_count(dir),
        }
    }
}

fn walk_count(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += walk_count(&path);
        } else if path.is_file() {
            total += 1;
        }
    }
    total
}

/// The loaded game, ready to run.
struct LoadedGame {
    world: World,
    /// Kept for the render loop / future asset streaming.
    #[allow(dead_code, reason = "held for the scene-render slice still to come")]
    assets: Option<AssetSource>,
    /// Whether the scene defines at least one camera.
    has_camera: bool,
}

/// Loads `export_dir`'s scene and assets into a `World`.
fn load(export_dir: &Path) -> LoadedGame {
    let assets = AssetSource::resolve(export_dir);
    if let Some(source) = &assets {
        tracing::info!(assets = source.count(), "asset source ready");
    } else {
        tracing::warn!("no assets.pak or assets/ directory found");
    }

    let mut world = World::new();
    let mut has_camera = false;

    let scene_path = export_dir.join("main.ron");
    // The scene can come from the bundle too; a loose `main.ron` wins for
    // quick iteration on an export.
    let scene_text = std::fs::read_to_string(&scene_path).ok().or_else(|| {
        assets
            .as_ref()
            .and_then(|source| source.read("main.ron"))
            .and_then(|bytes| String::from_utf8(bytes).ok())
    });

    match scene_text {
        Some(text) => match Scene::from_ron_str(&text) {
            Ok(scene) => match scene.validate() {
                Ok(()) => {
                    let spawned = scene.instantiate(&mut world);
                    has_camera = scene.entities.iter().any(|entity| entity.camera.is_some());
                    tracing::info!(entities = spawned.len(), has_camera, "scene instantiated");
                }
                Err(err) => {
                    tracing::error!(error = %err, "scene failed validation; starting empty")
                }
            },
            Err(err) => tracing::error!(error = %err, "scene is not valid RON; starting empty"),
        },
        None => tracing::warn!("no main.ron scene; starting empty"),
    }

    LoadedGame {
        world,
        assets,
        has_camera,
    }
}

/// Platform handler: owns the GPU context and clears the screen each
/// frame. The `World` is loaded up front so a real render pass can be
/// added without touching startup.
struct PlayerHandler {
    game: LoadedGame,
    gpu: Option<GpuContext>,
    clear: [f64; 3],
}

impl PlatformHandler for PlayerHandler {
    fn on_window_ready(&mut self, window: Arc<Window>) {
        match GpuContext::new(window) {
            Ok(gpu) => {
                tracing::info!("GPU context ready");
                self.gpu = Some(gpu);
            }
            Err(err) => tracing::error!(error = %err, "failed to initialise the GPU"),
        }
        let entity_count = self.game.world.iter_entities().count();
        tracing::info!(
            entities = entity_count,
            has_camera = self.game.has_camera,
            "player running"
        );
    }

    fn on_event(&mut self, event: PlatformEvent) {
        match event {
            PlatformEvent::Resized { width, height } => {
                if let Some(gpu) = &mut self.gpu
                    && let Err(err) = gpu.resize(width, height)
                {
                    tracing::warn!(error = %err, "resize failed");
                }
            }
            PlatformEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

impl PlayerHandler {
    fn render(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let [r, g, b] = self.clear;
        let result = gpu.render_with(|_device, _queue, encoder, view| {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("player clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        });
        if let Err(err) = result {
            tracing::error!(error = %err, "frame render failed");
        }
    }
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

    let game = load(&export_dir);

    let config = WindowConfig::new("RustyEngine Player".to_string(), 1280, 720);
    let handler = PlayerHandler {
        game,
        gpu: None,
        clear: [0.05, 0.06, 0.09],
    };
    if let Err(err) = run_windowed(config, handler) {
        tracing::error!(error = %err, "player exited with an error");
        std::process::exit(1);
    }
}
