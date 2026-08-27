//! VGE editor: a standalone window running the egui-based editor UI on
//! top of the engine's own renderer/platform layers.
//!
//! Starts from a small placeholder scene, editable from there on: the
//! "Scene View" panel renders every entity in it (each a plain lit
//! cube — see `engine_editor::viewport`'s module docs for why not a real
//! mesh yet) with a draggable move gizmo on whichever's selected;
//! "Hierarchy" lists that same scene's entities and can spawn/delete
//! them; "Inspector" edits the selected one's name/transform; "Assets"
//! lists files under `./assets` (relative to wherever the editor is run
//! from); "Console" shows captured log output.

use std::path::Path;
use std::sync::Arc;

use engine::ecs::components as ecs_components;
use engine::ecs::components::Name;
use engine::ecs::prelude::{ChildOf, World};
use engine::prelude::*;

/// Builds the starting scene: a "Parent Cube" with one child "Child
/// Cube", plus an independent "Light" root — enough to exercise multiple
/// roots, a parent/child pair, and a leaf with no children. Each has a
/// `Transform`, so the inspector panel has translation/rotation/scale to
/// edit and the Scene View has a position to draw each at. From here,
/// the hierarchy panel's "+ Cube"/"Delete" buttons add to or shrink this
/// same world live.
fn placeholder_world() -> World {
    let mut world = World::new();
    let parent = world
        .spawn((
            Name::new("Parent Cube"),
            ecs_components::Transform::default(),
        ))
        .id();
    world.spawn((
        Name::new("Child Cube"),
        ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
            1.5, 0.0, 0.0,
        ))),
        ChildOf(parent),
    ));
    world.spawn((
        Name::new("Light"),
        ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
            0.0, 3.0, 0.0,
        ))),
    ));
    world
}

/// Directory the asset browser panel scans, relative to the editor's
/// working directory — run the editor from a project's root so this
/// resolves to that project's own `assets/`.
const ASSETS_DIR: &str = "assets";

/// [`PlatformHandler`] that stands up a GPU context, an [`EditorShell`],
/// and a [`Viewport`], and draws the shell's UI (which shows the
/// viewport's rendered content plus the hierarchy/inspector/assets
/// panels) every frame.
struct EditorHandler {
    gpu: Option<GpuContext>,
    shell: Option<EditorShell>,
    viewport: Option<Viewport>,
    window: Option<Arc<Window>>,
    state: EditorState,
}

impl PlatformHandler for EditorHandler {
    fn on_window_ready(&mut self, window: Arc<Window>) {
        tracing::info!("editor window ready");
        let gpu = match GpuContext::new(Arc::clone(&window)) {
            Ok(gpu) => gpu,
            Err(err) => {
                tracing::error!(error = %err, "failed to initialize GPU context");
                return;
            }
        };
        let mut shell = EditorShell::new(&window, gpu.device(), gpu.config().format);
        let viewport = match Viewport::new(&gpu, &mut shell, 640, 480) {
            Ok(viewport) => viewport,
            Err(err) => {
                tracing::error!(error = %err, "failed to create editor viewport");
                return;
            }
        };

        self.gpu = Some(gpu);
        self.shell = Some(shell);
        self.viewport = Some(viewport);
        self.window = Some(window);
    }

    fn on_raw_window_event(&mut self, window: &Window, event: &WindowEvent) {
        if let Some(shell) = &mut self.shell {
            // Consumed-or-not is unused for now — there's no other
            // input-reactive system in this binary yet to defer to egui.
            let _ = shell.handle_window_event(window, event);
        }
    }

    fn on_event(&mut self, event: PlatformEvent) {
        match event {
            PlatformEvent::CloseRequested => tracing::info!("close requested"),
            PlatformEvent::Resized { width, height } => {
                if let Some(gpu) = &mut self.gpu
                    && let Err(err) = gpu.resize(width, height)
                {
                    tracing::warn!(error = %err, "skipped surface resize");
                }
            }
            PlatformEvent::RedrawRequested => {
                let (Some(gpu), Some(shell), Some(viewport), Some(window)) =
                    (&self.gpu, &mut self.shell, &self.viewport, &self.window)
                else {
                    return;
                };

                let entity_transforms = self.state.entity_transforms();
                viewport.render(gpu, &entity_transforms, self.state.gizmo_origin());
                let mut full_output = shell.run_frame(window, viewport, &mut self.state);
                // Unconditional, even though the frame itself might not
                // get drawn below (surface acquisition can skip a frame) —
                // see `EditorShell::apply_texture_updates`'s docs for why
                // texture handling can't share that fate.
                shell.apply_texture_updates(gpu.device(), gpu.queue(), &mut full_output);

                let size = window.inner_size();
                let screen_descriptor = ScreenDescriptor {
                    size_in_pixels: [size.width, size.height],
                    pixels_per_point: window.scale_factor() as f32,
                };

                let result = gpu.render_with(|device, queue, encoder, view| {
                    shell.render(
                        device,
                        queue,
                        encoder,
                        view,
                        &screen_descriptor,
                        full_output,
                    );
                });
                if let Err(err) = result {
                    tracing::error!(error = %err, "editor frame render failed");
                }
            }
            PlatformEvent::KeyboardInput { .. }
            | PlatformEvent::MouseButtonInput { .. }
            | PlatformEvent::CursorMoved { .. }
            | PlatformEvent::MouseWheel { .. } => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let console_log = ConsoleLog::new();
    logging::init_default_with_layer(ConsoleLayer::new(console_log.clone()))?;

    let config = EngineConfig::new("VGE Editor", env!("CARGO_PKG_VERSION"));
    let mut app = App::new(config)?;

    let assets = match scan_assets(Path::new(ASSETS_DIR)) {
        Ok(assets) => assets,
        Err(err) => {
            // A missing/unreadable assets directory shouldn't stop the
            // editor from opening — the panel just shows nothing.
            tracing::warn!(error = %err, dir = ASSETS_DIR, "failed to scan assets directory");
            Vec::new()
        }
    };

    let window_config = WindowConfig::new(app.config().app_name.clone(), 1280, 720);
    run_windowed(
        window_config,
        EditorHandler {
            gpu: None,
            shell: None,
            viewport: None,
            window: None,
            state: EditorState::new(placeholder_world(), assets, console_log),
        },
    )?;

    app.shutdown()?;
    Ok(())
}
