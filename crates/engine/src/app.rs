//! Rust gameplay API: a [`Game`] trait plus [`run_game`], so a game
//! project is a type that implements a few hooks instead of hand-wiring
//! the window, GPU, render pipelines, ECS world, and frame loop.
//!
//! [`run_game`] owns the window ([`engine_platform::run_windowed`]), a
//! [`GpuContext`], the standard 3D render setup (PBR + shadow + skybox +
//! HDR + post-processing), an [`Ecs`] world, [`RenderAssets`], a
//! [`Camera`] + [`LightSet`], an [`engine_platform::InputState`], and a
//! fixed-timestep clock. Each frame it applies input, runs the game's fixed and
//! variable update hooks, syncs the camera/light GPU bindings, and calls
//! [`engine_ecs::extract_and_render`].
//!
//! Everything the framework doesn't drive automatically (particles,
//! vegetation, terrain, navigation, sprites, debug lines) is still
//! reachable through [`GameContext`]'s accessors ([`GameContext::gpu`],
//! [`GameContext::assets_mut`], [`GameContext::world_mut`]); wiring those
//! into the frame loop is future work. A game needing a non-standard
//! render setup drops to the subsystem crates directly (all re-exported
//! from [`crate::prelude`]).
//!
//! ```no_run
//! use engine::prelude::*;
//!
//! struct MyGame;
//!
//! impl Game for MyGame {
//!     fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
//!         let (vertices, indices) = cube();
//!         let mesh = ctx
//!             .gpu()
//!             .create_mesh("cube", &vertices, &indices)
//!             .map_err(|e| GameError::Setup(e.to_string()))?;
//!         let texture = ctx.gpu().create_texture_from_rgba(
//!             "white",
//!             1,
//!             1,
//!             &[255, 255, 255, 255],
//!         );
//!         ctx.spawn_mesh(mesh, &texture, Material::default(), Transform::IDENTITY);
//!         Ok(())
//!     }
//!
//!     fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
//!         if ctx.input().is_key_held(KeyCode::Escape) {
//!             ctx.request_exit();
//!         }
//!         let _ = dt;
//!     }
//! }
//!
//! run_game(GameConfig::new("My Game", 1280, 720), MyGame)?;
//! # Ok::<(), GameError>(())
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use engine_core::FixedTimestep;
use engine_ecs::Ecs;
use engine_ecs::prelude::{Bundle, Entity};
use engine_platform::{
    PlatformError, PlatformEvent, PlatformHandler, Window, WindowConfig, run_windowed,
};
use engine_renderer::{
    Camera, CameraBinding, DirectionalLight, GpuContext, HdrTarget, InstancedPipeline, LightSet,
    LightsBinding, Material, Mesh, ParticleCameraBinding, ParticleFrame, ParticlePipelines,
    Pipeline, PostProcessStack, PostSettings, RenderAssets, RendererError, ShadowMap,
    ShadowPipeline, SkinnedMesh, SkinnedPipeline, SkyboxBinding, SkyboxPipeline, Texture,
    VegetationPipeline, Vertex, Wind, WindBinding, directional_light_view_projection,
    skybox_uniform,
};
use engine_utils::{AssetHandle, Transform};
use glam::Vec3;

/// Errors from [`run_game`] and its setup.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GameError {
    /// [`GameConfig::validate`] rejected the config.
    #[error("invalid game config: {0}")]
    InvalidConfig(String),
    /// The windowing / event-loop layer failed.
    #[error("platform error: {0}")]
    Platform(#[from] PlatformError),
    /// The renderer failed to initialize (GPU adapter/device/surface).
    #[error("renderer error: {0}")]
    Renderer(#[from] RendererError),
    /// The game's own [`Game::setup`] returned an error. Carries the
    /// game-supplied message.
    #[error("game setup failed: {0}")]
    Setup(String),
}

/// Read-only per-frame timing, from [`GameContext::time`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Time {
    /// Seconds since the first frame.
    pub elapsed: f32,
    /// Seconds the previous frame took (0 on the first frame, and for a
    /// glitched clock delta).
    pub delta: f32,
    /// Frames rendered so far (this is frame `frame`, 0-indexed at the
    /// first `update`).
    pub frame: u64,
}

impl Time {
    fn advance(&mut self, frame_seconds: f32) {
        let delta = if frame_seconds.is_finite() && frame_seconds >= 0.0 {
            frame_seconds
        } else {
            0.0
        };
        self.delta = delta;
        self.elapsed += delta;
        self.frame = self.frame.saturating_add(1);
    }
}

/// Window title/size plus the engine defaults a game can override before
/// [`run_game`].
#[derive(Debug, Clone)]
pub struct GameConfig {
    /// OS window title. Must not be empty/whitespace.
    pub title: String,
    /// Initial window width in logical pixels. Must be nonzero.
    pub width: u32,
    /// Initial window height in logical pixels. Must be nonzero.
    pub height: u32,
    /// Fixed-update rate in steps per second. Must be finite and
    /// positive.
    pub fixed_hz: f32,
    /// Post-processing settings the stack is built with.
    pub post: PostSettings,
    /// Starting camera. `aspect_ratio` is overwritten to match the window
    /// at startup and on resize.
    pub camera: Camera,
    /// Starting lights. The first directional light (if any) drives the
    /// shadow map and skybox sun.
    pub lights: LightSet,
}

impl GameConfig {
    /// A config for a `title` window of `width` x `height`, with engine
    /// defaults: 60 Hz fixed update, [`PostSettings::default`], a camera
    /// looking at the origin from `(3, 3, 6)`, and one sun-like
    /// directional light.
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        let aspect = if height == 0 {
            1.0
        } else {
            width as f32 / height as f32
        };
        let mut lights = LightSet::new();
        lights.directional.push(DirectionalLight {
            direction: Vec3::new(-0.4, -1.0, -0.3),
            color: Vec3::new(1.0, 0.98, 0.92),
            intensity: 3.0,
        });
        Self {
            title: title.into(),
            width,
            height,
            fixed_hz: 60.0,
            post: PostSettings::default(),
            camera: Camera::new(Vec3::new(3.0, 3.0, 6.0), Vec3::ZERO, aspect),
            lights,
        }
    }

    /// Checks the fields [`run_game`] can't recover from.
    ///
    /// # Errors
    ///
    /// [`GameError::InvalidConfig`] if `title` is empty/whitespace,
    /// `width`/`height` is `0`, or `fixed_hz` is not finite and positive.
    pub fn validate(&self) -> Result<(), GameError> {
        if self.title.trim().is_empty() {
            return Err(GameError::InvalidConfig("title must not be empty".into()));
        }
        if self.width == 0 || self.height == 0 {
            return Err(GameError::InvalidConfig(
                "window width and height must be nonzero".into(),
            ));
        }
        if !(self.fixed_hz.is_finite() && self.fixed_hz > 0.0) {
            return Err(GameError::InvalidConfig(
                "fixed_hz must be finite and positive".into(),
            ));
        }
        Ok(())
    }
}

/// A game: implement the hooks, hand it to [`run_game`].
///
/// `setup` runs once after the window and renderer are ready. `update`
/// runs once per rendered frame; `fixed_update` runs zero or more times
/// per frame at a fixed rate (for anything that integrates over time).
/// `on_event` sees each [`PlatformEvent`] as it arrives.
pub trait Game {
    /// One-time initialization: spawn entities, load assets, place the
    /// camera. Returning `Err` aborts [`run_game`] with that error.
    ///
    /// # Errors
    ///
    /// Whatever the game decides is fatal — typically wrapped in
    /// [`GameError::Setup`].
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError>;

    /// Per-frame update. `dt` is the real seconds since the last frame.
    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32);

    /// Fixed-rate update, run a whole number of times per frame at
    /// `step` seconds each (see [`GameConfig::fixed_hz`]). Zero calls on a
    /// fast frame is normal. Default: no-op.
    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, step: f32) {
        let _ = (ctx, step);
    }

    /// Called for every platform event (input, resize, close) before the
    /// engine's own handling of it. Default: no-op.
    fn on_event(&mut self, ctx: &mut GameContext<'_>, event: &PlatformEvent) {
        let _ = (ctx, event);
    }
}

/// Everything [`run_game`] owns for a running game. `GameContext` is a
/// borrow view over this; it is never handed out directly.
struct Runtime {
    gpu: GpuContext,
    ecs: Ecs,
    assets: RenderAssets,
    input: engine_platform::InputState,
    time: Time,
    ticker: FixedTimestep,
    exit_requested: bool,

    pipeline: Pipeline,
    shadow_pipeline: ShadowPipeline,
    shadow_map: ShadowMap,
    skinned_pipeline: SkinnedPipeline,
    instanced_pipeline: InstancedPipeline,
    skybox_pipeline: SkyboxPipeline,
    skybox: SkyboxBinding,
    hdr_target: HdrTarget,
    post: PostProcessStack,
    post_settings: PostSettings,
    camera_binding: CameraBinding,
    lights_binding: LightsBinding,

    particle_pipelines: ParticlePipelines,
    particle_camera: ParticleCameraBinding,
    vegetation_pipeline: VegetationPipeline,
    wind: Wind,
    wind_binding: WindBinding,
    wind_time: f32,

    camera: Camera,
    lights: LightSet,
}

impl Runtime {
    /// The direction/color of the first directional light, or a
    /// straight-down white fallback if the game removed all of them.
    fn sun(&self) -> (Vec3, Vec3) {
        self.lights
            .directional
            .first()
            .map(|light| (light.direction, light.color))
            .unwrap_or((Vec3::NEG_Y, Vec3::ONE))
    }
}

/// The game-facing handle passed to every [`Game`] hook. Borrows the
/// engine's runtime state for the duration of the call.
pub struct GameContext<'a> {
    runtime: &'a mut Runtime,
}

impl GameContext<'_> {
    /// The ECS world, for queries.
    pub fn world(&self) -> &engine_ecs::prelude::World {
        self.runtime.ecs.world()
    }

    /// The ECS world, mutable — spawn/despawn/insert directly, or use the
    /// `spawn_*` helpers.
    pub fn world_mut(&mut self) -> &mut engine_ecs::prelude::World {
        self.runtime.ecs.world_mut()
    }

    /// The [`Ecs`] wrapper (for running its stage schedules).
    pub fn ecs_mut(&mut self) -> &mut Ecs {
        &mut self.runtime.ecs
    }

    /// This frame's input snapshot.
    pub fn input(&self) -> &engine_platform::InputState {
        &self.runtime.input
    }

    /// The active camera. Mutate it here; the engine re-uploads its GPU
    /// binding after `update` each frame.
    pub fn camera_mut(&mut self) -> &mut Camera {
        &mut self.runtime.camera
    }

    /// The scene lights. Mutate them here; the engine re-uploads the GPU
    /// binding after `update` each frame. The first directional light
    /// drives the shadow map and skybox sun.
    pub fn lights_mut(&mut self) -> &mut LightSet {
        &mut self.runtime.lights
    }

    /// The GPU context, for creating meshes/textures/materials.
    pub fn gpu(&self) -> &GpuContext {
        &self.runtime.gpu
    }

    /// The GPU-resident mesh/material store that `MeshRenderer` handles
    /// resolve against.
    pub fn assets_mut(&mut self) -> &mut RenderAssets {
        &mut self.runtime.assets
    }

    /// The PBR pipeline, for building bindings against
    /// (`gpu().create_material_binding(ctx.pipeline(), ...)`).
    pub fn pipeline(&self) -> &Pipeline {
        &self.runtime.pipeline
    }

    /// The GPU vertex-skinning pipeline (for building a `SkinnedBinding`
    /// against, or a `SkinnedMesh`).
    pub fn skinned_pipeline(&self) -> &SkinnedPipeline {
        &self.runtime.skinned_pipeline
    }

    /// The global wind that animates every `VegetationRenderer`. Mutate it
    /// here; the engine re-uploads its GPU binding each frame.
    pub fn wind_mut(&mut self) -> &mut Wind {
        &mut self.runtime.wind
    }

    /// This frame's timing.
    pub fn time(&self) -> Time {
        self.runtime.time
    }

    /// Ask the engine to close the window and end [`run_game`] after this
    /// frame.
    pub fn request_exit(&mut self) {
        self.runtime.exit_requested = true;
    }

    /// Spawns an entity from any `bevy_ecs` bundle. Returns its
    /// [`Entity`].
    pub fn spawn(&mut self, bundle: impl Bundle) -> Entity {
        self.runtime.ecs.world_mut().spawn(bundle).id()
    }

    /// Uploads `mesh` and a material binding from `texture`/`material`,
    /// then spawns a `(Transform, MeshRenderer)` entity for it. Returns
    /// its [`Entity`].
    pub fn spawn_mesh(
        &mut self,
        mesh: Mesh,
        texture: &Texture,
        material: Material,
        transform: Transform,
    ) -> Entity {
        let renderer = engine_ecs::components::MeshRenderer::new(
            &self.runtime.gpu,
            &self.runtime.pipeline,
            &mut self.runtime.assets,
            mesh,
            texture,
            material,
        );
        self.runtime
            .ecs
            .world_mut()
            .spawn((engine_ecs::components::Transform::from(transform), renderer))
            .id()
    }

    /// Like [`GameContext::spawn_mesh`] but for a single instanced draw of
    /// `mesh` at every transform in `instances`.
    pub fn spawn_mesh_instanced(
        &mut self,
        mesh: Mesh,
        texture: &Texture,
        material: Material,
        instances: Vec<Transform>,
    ) -> Entity {
        let renderer = engine_ecs::components::InstancedMeshRenderer::new(
            &self.runtime.gpu,
            &self.runtime.pipeline,
            &mut self.runtime.assets,
            mesh,
            texture,
            material,
            instances,
        );
        self.runtime.ecs.world_mut().spawn(renderer).id()
    }

    /// Uploads `mesh` and a material binding, then spawns a
    /// `(Transform, SkinnedMeshRenderer)` entity. The game uploads joint
    /// matrices each frame into the entity's `skin.joints_buffer` (see
    /// `engine_animation::compute_skinning_matrices`).
    pub fn spawn_skinned_mesh(
        &mut self,
        mesh: SkinnedMesh,
        texture: &Texture,
        material: Material,
        transform: Transform,
    ) -> Entity {
        let renderer = engine_ecs::components::SkinnedMeshRenderer::new(
            &self.runtime.gpu,
            &self.runtime.pipeline,
            &self.runtime.skinned_pipeline,
            &mut self.runtime.assets,
            mesh,
            texture,
            material,
        );
        self.runtime
            .ecs
            .world_mut()
            .spawn((engine_ecs::components::Transform::from(transform), renderer))
            .id()
    }

    /// Uploads `mesh` and a material binding, then spawns a
    /// `VegetationRenderer` entity that draws every transform in
    /// `instances` through the wind-animated pipeline. The mesh should be
    /// authored with its base at `y = 0` (roots stay put, tips sway).
    pub fn spawn_vegetation(
        &mut self,
        mesh: Mesh,
        texture: &Texture,
        material: Material,
        instances: Vec<Transform>,
    ) -> Entity {
        let renderer = engine_ecs::components::VegetationRenderer::new(
            &self.runtime.gpu,
            &self.runtime.pipeline,
            &mut self.runtime.assets,
            mesh,
            texture,
            material,
            instances,
        );
        self.runtime.ecs.world_mut().spawn(renderer).id()
    }

    /// Re-uploads `vertices` into the mesh behind `handle` in place — for
    /// a mesh created with `GpuContext::create_mesh_dynamic` whose vertex
    /// count is unchanged (e.g. a sculpted `Heightmap`'s terrain mesh).
    /// Returns `false` (and logs a warning) if the handle isn't registered
    /// or the write fails.
    pub fn rewrite_mesh(&mut self, handle: AssetHandle<Mesh>, vertices: &[Vertex]) -> bool {
        let Some(mesh) = self.runtime.assets.meshes.get_mut(handle) else {
            tracing::warn!("rewrite_mesh: handle not registered");
            return false;
        };
        match self.runtime.gpu.write_mesh_vertices(mesh, vertices) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(error = %err, "rewrite_mesh: write failed");
                false
            }
        }
    }
}

/// The internal [`PlatformHandler`]. Owns the game and (once the window is
/// ready) the [`Runtime`]; stashes any fatal error into `error_slot` for
/// [`run_game`] to surface after the loop exits.
struct GameRunner<G: Game> {
    config: Option<GameConfig>,
    game: G,
    runtime: Option<Runtime>,
    error_slot: Rc<Cell<Option<GameError>>>,
    last_frame: Option<Instant>,
    finished: bool,
}

impl<G: Game> GameRunner<G> {
    fn fail(&mut self, error: GameError) {
        self.error_slot.set(Some(error));
        self.finished = true;
    }
}

impl<G: Game> PlatformHandler for GameRunner<G> {
    fn on_window_ready(&mut self, window: std::sync::Arc<Window>) {
        let Some(config) = self.config.take() else {
            return;
        };
        let mut runtime = match build_runtime(window, &config) {
            Ok(runtime) => runtime,
            Err(err) => {
                self.fail(err);
                return;
            }
        };
        if let Err(err) = self.game.setup(&mut GameContext {
            runtime: &mut runtime,
        }) {
            self.fail(err);
            return;
        }
        self.runtime = Some(runtime);
    }

    fn on_event(&mut self, event: PlatformEvent) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };

        match event {
            PlatformEvent::Resized { width, height } => {
                if let Err(err) = runtime.gpu.resize(width, height) {
                    tracing::warn!(error = %err, "skipped surface resize");
                } else {
                    runtime.camera.aspect_ratio = width as f32 / height.max(1) as f32;
                    runtime.hdr_target = runtime.gpu.create_hdr_target(width, height);
                    runtime.post = runtime.gpu.create_post_process_stack(
                        &runtime.hdr_target,
                        width,
                        height,
                        runtime.post_settings,
                    );
                }
                self.game.on_event(
                    &mut GameContext {
                        runtime: &mut *runtime,
                    },
                    &event,
                );
            }
            PlatformEvent::RedrawRequested => {
                let now = Instant::now();
                let frame_seconds = self
                    .last_frame
                    .map(|last| now.duration_since(last).as_secs_f32())
                    .unwrap_or(0.0);
                self.last_frame = Some(now);

                runtime.time.advance(frame_seconds);

                let steps = runtime.ticker.advance(frame_seconds);
                let step_seconds = runtime.ticker.step_seconds();
                for _ in 0..steps {
                    self.game.fixed_update(
                        &mut GameContext {
                            runtime: &mut *runtime,
                        },
                        step_seconds,
                    );
                }
                self.game.update(
                    &mut GameContext {
                        runtime: &mut *runtime,
                    },
                    frame_seconds,
                );
                runtime.input.end_frame();

                render_frame(runtime, frame_seconds);
            }
            other => {
                runtime.input.apply_event(&other);
                self.game.on_event(
                    &mut GameContext {
                        runtime: &mut *runtime,
                    },
                    &other,
                );
            }
        }
    }

    fn should_exit(&self) -> bool {
        self.finished
            || self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.exit_requested)
    }
}

/// Builds the standard 3D render setup and an empty world from `config`.
fn build_runtime(
    window: std::sync::Arc<Window>,
    config: &GameConfig,
) -> Result<Runtime, GameError> {
    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);

    let gpu = GpuContext::new(window)?;

    let pipeline = gpu.create_pbr_pipeline("pbr");
    let shadow_pipeline = gpu.create_shadow_pipeline(&pipeline, "shadow");
    let skinned_pipeline = gpu.create_skinned_pbr_pipeline(&pipeline, "skinned pbr");
    let instanced_pipeline = gpu.create_instanced_pbr_pipeline(&pipeline, "instanced pbr");
    let skybox_pipeline = gpu.create_skybox_pipeline("skybox");

    let mut camera = config.camera;
    camera.aspect_ratio = width as f32 / height as f32;
    let lights = config.lights.clone();

    let (sun_direction, sun_color) = lights
        .directional
        .first()
        .map(|light| (light.direction, light.color))
        .unwrap_or((Vec3::NEG_Y, Vec3::ONE));

    let light_space_matrix = directional_light_view_projection(sun_direction, Vec3::ZERO, 8.0);
    let shadow_map = gpu.create_shadow_map(&shadow_pipeline, light_space_matrix);

    let camera_binding = gpu.create_camera_binding(&pipeline, &camera.to_uniform());
    let lights_binding = gpu.create_lights_binding(&pipeline, &lights.to_uniform(), &shadow_map);
    let skybox = gpu.create_skybox_binding(
        &skybox_pipeline,
        &skybox_uniform(&camera, sun_direction, sun_color),
    );

    let hdr_target = gpu.create_hdr_target(width, height);
    let post = gpu.create_post_process_stack(&hdr_target, width, height, config.post);

    let particle_pipelines = gpu.create_particle_pipelines("particles");
    let particle_camera = gpu.create_particle_camera_binding(&particle_pipelines, &camera);
    let vegetation_pipeline = gpu.create_vegetation_pipeline(&pipeline, "vegetation");
    let wind = Wind::BREEZE;
    let wind_binding = gpu.create_wind_binding(&vegetation_pipeline, &wind, 0.0);

    let ticker = FixedTimestep::from_hz(config.fixed_hz)
        .map_err(|err| GameError::InvalidConfig(err.to_string()))?;

    Ok(Runtime {
        gpu,
        ecs: Ecs::new(),
        assets: RenderAssets::new(),
        input: engine_platform::InputState::new(),
        time: Time::default(),
        ticker,
        exit_requested: false,
        pipeline,
        shadow_pipeline,
        shadow_map,
        skinned_pipeline,
        instanced_pipeline,
        skybox_pipeline,
        skybox,
        hdr_target,
        post,
        post_settings: config.post,
        camera_binding,
        lights_binding,
        particle_pipelines,
        particle_camera,
        vegetation_pipeline,
        wind,
        wind_binding,
        wind_time: 0.0,
        camera,
        lights,
    })
}

/// Advances particle emitters and wind, syncs the camera / light /
/// skybox / particle-camera / wind GPU bindings from the game's current
/// state, then renders the world (opaque meshes, skinned, instanced,
/// wind-animated vegetation, and particles).
fn render_frame(runtime: &mut Runtime, dt: f32) {
    // Emitters step here so a game only has to spawn a `ParticleEmitter`
    // to get particles; wind advances on a bounded clock so `sin` stays
    // precise.
    engine_ecs::update_particles(runtime.ecs.world_mut(), dt);
    runtime.wind_time = (runtime.wind_time + dt.max(0.0)) % 1000.0;
    runtime
        .gpu
        .write_wind(&runtime.wind_binding, &runtime.wind, runtime.wind_time);

    runtime
        .gpu
        .write_uniform_buffer(&runtime.camera_binding.buffer, &runtime.camera.to_uniform());
    runtime
        .gpu
        .write_uniform_buffer(&runtime.lights_binding.buffer, &runtime.lights.to_uniform());
    let (sun_direction, sun_color) = runtime.sun();
    runtime.gpu.write_uniform_buffer(
        &runtime.skybox.buffer,
        &skybox_uniform(&runtime.camera, sun_direction, sun_color),
    );
    runtime
        .gpu
        .write_particle_camera_binding(&runtime.particle_camera, &runtime.camera);

    // Owned here so the slices outlive the render call that uploads them.
    let (alpha_particles, additive_particles) =
        engine_ecs::extract_particles(runtime.ecs.world_mut());
    let particle_frame = ParticleFrame {
        pipelines: &runtime.particle_pipelines,
        camera: &runtime.particle_camera,
        alpha: &alpha_particles,
        additive: &additive_particles,
    };
    let particles = (!particle_frame.is_empty()).then_some(particle_frame);

    let frustum = runtime.camera.frustum();
    if let Err(err) = engine_ecs::extract_and_render(
        runtime.ecs.world_mut(),
        &runtime.assets,
        &runtime.gpu,
        &runtime.pipeline,
        &runtime.shadow_pipeline,
        &runtime.shadow_map,
        &runtime.skybox_pipeline,
        &runtime.skybox,
        &runtime.hdr_target,
        &runtime.post,
        &runtime.camera_binding,
        &frustum,
        &runtime.lights_binding,
        &runtime.skinned_pipeline,
        &runtime.instanced_pipeline,
        None,
        particles,
        Some((&runtime.vegetation_pipeline, &runtime.wind_binding)),
    ) {
        tracing::error!(error = %err, "frame render failed");
    }
}

/// Opens a window per `config` and runs `game` until it calls
/// [`GameContext::request_exit`] or the window is closed. Blocks the
/// calling thread; call it from `main`.
///
/// # Errors
///
/// - [`GameError::InvalidConfig`] if [`GameConfig::validate`] fails.
/// - [`GameError::Platform`] if the event loop or window can't be created.
/// - [`GameError::Renderer`] if GPU initialization fails.
/// - [`GameError::Setup`] (or whatever the game returns) if
///   [`Game::setup`] fails.
pub fn run_game(config: GameConfig, game: impl Game + 'static) -> Result<(), GameError> {
    config.validate()?;

    let window_config = WindowConfig::new(config.title.clone(), config.width, config.height);
    // `run_windowed` takes the handler by value and can't hand a setup
    // failure back, so the handler drops any fatal error into this shared
    // slot (single-threaded; genuine shared ownership between here and the
    // moved-in handler).
    let error_slot: Rc<Cell<Option<GameError>>> = Rc::new(Cell::new(None));
    let runner = GameRunner {
        config: Some(config),
        game,
        runtime: None,
        error_slot: Rc::clone(&error_slot),
        last_frame: None,
        finished: false,
    };

    run_windowed(window_config, runner)?;

    match error_slot.take() {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_new_is_valid_and_sizes_the_camera() {
        let config = GameConfig::new("Test", 800, 400);
        assert!(config.validate().is_ok());
        assert!((config.camera.aspect_ratio - 2.0).abs() < 1e-6);
        assert_eq!(config.fixed_hz, 60.0);
    }

    #[test]
    fn config_validate_rejects_bad_fields() {
        let mut config = GameConfig::new("Test", 800, 600);
        config.title = "   ".to_string();
        assert!(matches!(
            config.validate(),
            Err(GameError::InvalidConfig(_))
        ));

        let mut config = GameConfig::new("Test", 0, 600);
        assert!(config.validate().is_err());
        config.width = 640;
        config.height = 0;
        assert!(config.validate().is_err());

        let mut config = GameConfig::new("Test", 800, 600);
        config.fixed_hz = 0.0;
        assert!(config.validate().is_err());
        config.fixed_hz = f32::NAN;
        assert!(config.validate().is_err());
        config.fixed_hz = -30.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn time_advance_accumulates_and_ignores_bad_deltas() {
        let mut time = Time::default();
        time.advance(0.5);
        time.advance(0.25);
        assert!((time.elapsed - 0.75).abs() < 1e-6);
        assert!((time.delta - 0.25).abs() < 1e-6);
        assert_eq!(time.frame, 2);

        time.advance(f32::NAN);
        time.advance(-1.0);
        time.advance(f32::INFINITY);
        assert!((time.elapsed - 0.75).abs() < 1e-6);
        assert_eq!(time.delta, 0.0);
        assert_eq!(time.frame, 5);
    }

    #[test]
    fn game_error_wraps_lower_layer_errors() {
        let renderer_err: GameError = RendererError::EmptyMesh.into();
        assert!(matches!(renderer_err, GameError::Renderer(_)));
        assert!(renderer_err.to_string().contains("renderer error"));

        let setup = GameError::Setup("no save file".into());
        assert!(setup.to_string().contains("no save file"));
    }

    #[test]
    fn fixed_step_count_follows_the_ticker() {
        // The exact call the frame loop makes: advance the ticker by the
        // frame delta, run that many fixed steps.
        let mut ticker = FixedTimestep::from_hz(50.0).unwrap(); // 0.02 s
        assert_eq!(ticker.advance(0.05), 2);
        assert_eq!(ticker.advance(0.005), 0);
        assert_eq!(ticker.advance(0.02), 1);
    }
}
