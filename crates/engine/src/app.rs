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
//! Driven automatically each frame: particle emitters, wind-animated
//! vegetation, the camera/light/skybox GPU bindings, and — once per fixed
//! step — the physics world plus the pose sync back into `Transform`.
//!
//! Not yet driven by the loop: terrain, navigation, sprites, and runtime
//! UI. Those stay reachable through [`GameContext`]'s
//! accessors ([`GameContext::gpu`], [`GameContext::assets_mut`],
//! [`GameContext::world_mut`]) until their own tasks wire them in. A game
//! needing a non-standard render setup drops to the subsystem crates
//! directly (all re-exported from [`crate::prelude`]).
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

/// Maximum variable-step delta delivered to gameplay and presentation
/// systems. A stalled/resumed window must not teleport entities or trigger an
/// unbounded burst of work in one frame.
pub const MAX_FRAME_SECONDS: f32 = 0.1;

fn clamp_frame_seconds(seconds: f32) -> f32 {
    if seconds.is_finite() && seconds >= 0.0 {
        seconds.min(MAX_FRAME_SECONDS)
    } else {
        0.0
    }
}

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
        let delta = clamp_frame_seconds(frame_seconds);
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
    /// Gravity for the physics world, in world units per second squared.
    /// Must be finite. `Vec3::ZERO` is valid (a zero-g game).
    pub gravity: Vec3,
    /// Where this game's save lives. `None` disables saving entirely —
    /// the right choice for a demo or a test harness.
    pub save_path: Option<std::path::PathBuf>,
    /// When to persist. See [`SavePolicy`].
    pub save_policy: SavePolicy,
    /// Whether the frame loop drives UI focus from the keyboard and
    /// gamepad (arrow keys / d-pad / left stick to move, Enter or the
    /// south face button to activate). On by default, and inert while
    /// the UI has nothing focusable, so a game whose arrow keys drive
    /// gameplay is unaffected until it builds a menu. Set `false` to
    /// bind menu navigation yourself.
    pub ui_navigation: bool,
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
            gravity: Vec3::new(0.0, -9.81, 0.0),
            // Off unless a game names a file: writing saves nobody asked
            // for would be a surprise.
            save_path: None,
            save_policy: SavePolicy::default(),
            ui_navigation: true,
        }
    }

    /// Checks the fields [`run_game`] can't recover from.
    ///
    /// # Errors
    ///
    /// [`GameError::InvalidConfig`] if `title` is empty/whitespace,
    /// `width`/`height` is `0`, `fixed_hz` is not finite and positive, or
    /// `gravity` is not finite.
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
        if !self.gravity.is_finite() {
            return Err(GameError::InvalidConfig("gravity must be finite".into()));
        }
        Ok(())
    }
}

/// When the engine persists a game's state.
///
/// The *mechanism* (snapshot, atomic write, versioned load) belongs to
/// the engine; *when* to use it is a game's decision, and games differ.
/// A life sim persists continuously; a roguelike saves on death; a
/// puzzle game saves at checkpoints only. So this is a policy the game
/// picks, with the modern default already chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SavePolicy {
    /// Persist every `interval`, and once more on exit.
    ///
    /// The default. A player closes the game and reopens it where they
    /// left off, with no save button to remember — what most games have
    /// done since manual save slots stopped being necessary.
    Continuous {
        /// Seconds between automatic saves.
        interval: f32,
    },
    /// Persist only when the game exits cleanly.
    OnExit,
    /// Never automatically. The game calls
    /// [`GameContext::request_save`] itself.
    Manual,
}

impl SavePolicy {
    /// Whether this policy saves when the game closes.
    fn saves_on_exit(self) -> bool {
        matches!(self, SavePolicy::Continuous { .. } | SavePolicy::OnExit)
    }

    /// The autosave interval, if this policy has one.
    fn interval(self) -> Option<f32> {
        match self {
            SavePolicy::Continuous { interval } if interval > 0.0 => Some(interval),
            _ => None,
        }
    }
}

impl Default for SavePolicy {
    /// Autosave every 30 seconds, and on exit.
    fn default() -> Self {
        SavePolicy::Continuous { interval: 30.0 }
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
    ///
    /// Put anything that integrates over time here — physics forces,
    /// velocities, timers — not in [`Game::update`], whose `dt` varies
    /// with frame rate.
    ///
    /// # Ordering within one fixed step
    ///
    /// 1. this hook runs (apply forces / set velocities),
    /// 2. the physics world steps by `step`,
    /// 3. simulated poses are written into each body's `Transform`.
    ///
    /// So a `Transform` read *here* is the pose from the end of the
    /// previous step, while one read in [`Game::update`] is current.
    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, step: f32) {
        let _ = (ctx, step);
    }

    /// Called for every platform event (input, resize, close) before the
    /// engine's own handling of it. Default: no-op.
    fn on_event(&mut self, ctx: &mut GameContext<'_>, event: &PlatformEvent) {
        let _ = (ctx, event);
    }

    /// This game's own state, serialized, for the engine to persist.
    ///
    /// Score, inventory, quest flags, clock — whatever the world snapshot
    /// does not already carry. Returning `None` saves the world only.
    ///
    /// A string rather than a typed value so [`Game`] stays non-generic:
    /// the engine never needs to know a game's save shape, and a game can
    /// use RON, JSON, or anything else it likes.
    ///
    /// Default: nothing extra.
    fn save_data(&self) -> Option<String> {
        None
    }

    /// Restores what [`Game::save_data`] produced, called once after
    /// [`Game::setup`] when a save was found.
    ///
    /// After `setup` rather than before, so a game applies saved state to
    /// the world it has just built rather than racing it.
    ///
    /// Default: ignore it.
    fn load_data(&mut self, data: &str) {
        let _ = data;
    }
}

/// Everything [`run_game`] owns for a running game. `GameContext` is a
/// borrow view over this; it is never handed out directly.
struct Runtime {
    gpu: GpuContext,
    ecs: Ecs,
    physics: engine_physics::PhysicsWorld,
    /// `None` when the audio backend failed to start — the game runs
    /// silent rather than refusing to run.
    audio: Option<engine_audio::AudioContext>,
    /// Mixer buses the game asked for, indexed by
    /// `engine_ecs::AudioEmitter::bus`.
    buses: Vec<engine_audio::Bus>,
    /// The game's retained UI tree, laid out and drawn each frame.
    ui: engine_ui::Ui,
    /// Glyph sheet plus the white texel solid fills sample.
    glyph_atlas: engine_renderer::GlyphAtlas,
    ui_pipeline: engine_renderer::UiPipeline,
    /// Where the UI atlas's opaque white texel sits, for untextured
    /// quads.
    ui_white_uv: [f32; 4],
    /// Nodes clicked this frame, from the pre-`update` hit test plus any
    /// keyboard/gamepad activation.
    ui_clicked: Vec<engine_ui::NodeId>,
    /// Whether the loop drives UI focus from the keyboard and gamepad.
    ui_navigation: bool,
    /// Last frame's discrete left-stick direction, so a held stick moves
    /// focus once instead of every frame.
    ui_nav_stick: (i8, i8),
    /// Where this game's save lives, if it saves at all.
    save_path: Option<std::path::PathBuf>,
    /// When to persist.
    save_policy: SavePolicy,
    /// Seconds since the last automatic save.
    since_save: f32,
    /// Set by `GameContext::request_save`, honoured next frame.
    save_requested: bool,
    /// The world snapshot read at launch, if a save existed. Held so a
    /// game can restore it on its own terms.
    loaded_world: Option<engine_scene::WorldSnapshot>,
    assets: RenderAssets,
    input: engine_platform::InputState,
    /// Gamepad backend. Polled once per frame — gilrs has its own event
    /// loop and does not arrive through `winit`.
    gamepads: engine_platform::Gamepads,
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

    sprite_pipeline: engine_renderer::SpritePipeline,
    /// The unit quad every sprite instance stamps out. One upload for
    /// the whole game.
    sprite_quad: engine_renderer::Mesh,
    /// The atlas every `Sprite` samples, once the game has set one.
    /// `None` until then, which skips the sprite draw entirely.
    sprite_atlas: Option<engine_renderer::SpriteAtlasBinding>,
    /// Whether the "sprites spawned but no atlas set" warning has been
    /// logged. Once per run, not once per frame.
    warned_missing_atlas: bool,

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

/// The pieces needed to instantiate a scene, borrowed together.
///
/// See [`GameContext::scene_parts`] for why this exists rather than four
/// separate accessor calls.
pub struct SceneParts<'a> {
    /// The GPU context uploads go through.
    pub gpu: &'a GpuContext,
    /// The PBR pipeline material and model bindings are built against.
    pub pipeline: &'a Pipeline,
    /// The skinned variant, for `SkinnedMeshRenderer`.
    pub skinned_pipeline: &'a SkinnedPipeline,
    /// Where uploaded meshes and materials are registered.
    pub assets: &'a mut RenderAssets,
    /// The world entities are spawned into.
    pub world: &'a mut engine_ecs::prelude::World,
}

/// The game-facing handle passed to every [`Game`] hook. Borrows the
/// engine's runtime state for the duration of the call.
pub struct GameContext<'a> {
    runtime: &'a mut Runtime,
}

impl GameContext<'_> {
    /// The ECS world, for queries.
    /// Disjoint borrows of everything needed to turn a
    /// [`engine_scene::Scene`] into live entities: the GPU, the PBR
    /// pipeline, the render-asset store, and the world.
    ///
    /// Handed out together because they are needed together, and the
    /// per-field accessors can't be: `gpu()` borrows shared while
    /// `assets_mut()` and `world_mut()` borrow mutably, so the borrow
    /// checker rejects holding them at once. These are distinct fields of
    /// one struct, so splitting them in a single call is fine.
    ///
    /// The standalone player uses this to resolve an exported scene's
    /// asset references in [`Game::setup`].
    pub fn scene_parts(&mut self) -> SceneParts<'_> {
        SceneParts {
            gpu: &self.runtime.gpu,
            pipeline: &self.runtime.pipeline,
            skinned_pipeline: &self.runtime.skinned_pipeline,
            assets: &mut self.runtime.assets,
            world: self.runtime.ecs.world_mut(),
        }
    }

    /// Asks the engine to save at the end of this frame.
    ///
    /// The only way to save under [`SavePolicy::Manual`], and available
    /// under the others for a game that wants an extra save at a
    /// specific moment.
    pub fn request_save(&mut self) {
        self.runtime.save_requested = true;
    }

    /// The world snapshot read from the save at launch, if there was one.
    ///
    /// Opt-in rather than restored automatically: a game whose entities
    /// are generated in [`Game::setup`] — procedural terrain, scattered
    /// props — would get a second copy of everything if the engine
    /// respawned the snapshot on top. A game whose entities come from
    /// assets calls
    /// [`GameContext::restore_saved_world`] instead.
    pub fn saved_world(&self) -> Option<&engine_scene::WorldSnapshot> {
        self.runtime.loaded_world.as_ref()
    }

    /// Spawns the launch-time save's entities into the world.
    ///
    /// Returns `None` when there was no save. Does not clear the world
    /// first — a game that wants a replacement rather than a merge should
    /// clear it itself.
    pub fn restore_saved_world(
        &mut self,
        resolver: &mut impl engine_scene::SceneResolver,
    ) -> Option<engine_scene::InstantiateReport> {
        let snapshot = self.runtime.loaded_world.clone()?;
        Some(snapshot.restore(self.runtime.ecs.world_mut(), resolver))
    }

    /// Sets the atlas every [`engine_ecs::components::Sprite`] samples,
    /// replacing any previous one.
    ///
    /// Sprites are not drawn until a game calls this: the engine has no
    /// way to guess which texture a game's 2D art lives in. One atlas
    /// per game, so every sprite in a frame is one draw call — a game
    /// needing several sheets should pack them into one image and give
    /// each sheet its own regions.
    ///
    /// Call it in [`Game::setup`], or any time after (a level change
    /// swapping sheets is fine — the next frame uses the new one).
    pub fn set_sprite_atlas(&mut self, atlas: &engine_renderer::TextureAtlas) {
        self.runtime.sprite_atlas = Some(
            self.runtime
                .gpu
                .create_sprite_atlas_binding(&self.runtime.sprite_pipeline, atlas),
        );
    }

    /// Builds a physics body for every entity in `scene` that carries
    /// [`engine_scene::ColliderData`], attaching it to the matching
    /// entity from `report`.
    ///
    /// Returns how many bodies were created.
    ///
    /// ## Why this is a separate call
    ///
    /// `Scene::instantiate` lives in `engine_scene`, which has no
    /// physics dependency — deliberately, so the scene format stays
    /// plain data that the editor, the player and a headless tool can
    /// all read. Turning that data into rapier bodies needs a live
    /// physics world, which only the frame loop owns. So instantiate,
    /// then call this.
    ///
    /// ## Scale
    ///
    /// A primitive shape is scaled by its entity's transform, using the
    /// largest axis for shapes that cannot be scaled unevenly (a sphere
    /// has one radius). A [`engine_scene::ColliderShape::TriMesh`] is
    /// scaled per axis, since its vertices are.
    ///
    /// A trimesh whose mesh is missing from `library`, or whose
    /// triangles rapier rejects, is logged and skipped: one bad collider
    /// must not cost the rest of the level its collision.
    pub fn spawn_scene_colliders(
        &mut self,
        scene: &engine_scene::Scene,
        report: &engine_scene::InstantiateReport,
        library: &engine_scene::AssetLibrary,
    ) -> usize {
        use engine_scene::{BodyKind, ColliderShape};

        let mut built = 0;
        for (index, scene_entity) in scene.entities.iter().enumerate() {
            let Some(collider) = &scene_entity.collider else {
                continue;
            };
            let Some(&entity) = report.spawned.get(index) else {
                continue;
            };
            let transform = scene_entity
                .transform
                .map(engine_utils::Transform::from)
                .unwrap_or(engine_utils::Transform::IDENTITY);
            // One number for shapes with a single radius: a sphere
            // scaled 2x on X only is not a sphere, and silently picking
            // the wrong axis would put collision where the art is not.
            let uniform = transform.scale.max_element();

            let shape = match &collider.shape {
                ColliderShape::Ball { radius } => {
                    Some(engine_physics::ColliderBuilder::ball(radius * uniform))
                }
                ColliderShape::Cuboid { half_extents } => {
                    let scaled = glam::Vec3::from_array(*half_extents) * transform.scale;
                    Some(engine_physics::ColliderBuilder::cuboid(
                        scaled.x, scaled.y, scaled.z,
                    ))
                }
                ColliderShape::Cylinder {
                    half_height,
                    radius,
                } => Some(engine_physics::ColliderBuilder::cylinder(
                    half_height * transform.scale.y,
                    radius * uniform,
                )),
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => Some(engine_physics::ColliderBuilder::capsule_y(
                    half_height * transform.scale.y,
                    radius * uniform,
                )),
                ColliderShape::TriMesh { mesh } => {
                    match engine_scene::load_mesh_geometry(library, mesh) {
                        Ok(Some((vertices, indices))) => {
                            let points: Vec<glam::Vec3> = vertices
                                .iter()
                                .map(|vertex| {
                                    glam::Vec3::from_array(vertex.position) * transform.scale
                                })
                                .collect();
                            let triangles: Vec<[u32; 3]> = indices
                                .chunks_exact(3)
                                .map(|t| [t[0], t[1], t[2]])
                                .collect();
                            match engine_physics::ColliderBuilder::trimesh(points, triangles) {
                                Ok(builder) => Some(builder),
                                Err(err) => {
                                    tracing::warn!(
                                        entity = scene_entity.name.as_deref().unwrap_or("<unnamed>"),
                                        error = %err,
                                        "skipping collider: rapier rejected the mesh"
                                    );
                                    None
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::warn!(
                                entity = scene_entity.name.as_deref().unwrap_or("<unnamed>"),
                                mesh = %mesh.id,
                                "skipping collider: the project does not carry that mesh"
                            );
                            None
                        }
                        Err(err) => {
                            tracing::warn!(
                                entity = scene_entity.name.as_deref().unwrap_or("<unnamed>"),
                                error = %err,
                                "skipping collider: mesh could not be loaded"
                            );
                            None
                        }
                    }
                }
            };
            let Some(shape) = shape else { continue };

            let body = match collider.body {
                BodyKind::Fixed => engine_physics::RigidBodyBuilder::fixed(),
                BodyKind::Dynamic => engine_physics::RigidBodyBuilder::dynamic(),
                BodyKind::Kinematic => engine_physics::RigidBodyBuilder::kinematic_position_based(),
            }
            .translation(transform.translation)
            .rotation(transform.rotation.to_scaled_axis());

            let (rigid_body, collider_component) =
                engine_ecs::components::RigidBody::spawn(&mut self.runtime.physics, body, shape);
            self.runtime
                .ecs
                .world_mut()
                .entity_mut(entity)
                .insert((rigid_body, collider_component));
            built += 1;
        }

        tracing::info!(bodies = built, "spawned colliders from scene data");
        built
    }

    /// The game's UI tree. Build or mutate it here; the frame loop lays
    /// it out, feeds it pointer input, and draws it.
    pub fn ui_mut(&mut self) -> &mut engine_ui::Ui {
        &mut self.runtime.ui
    }

    /// The UI tree, for reading node rects.
    pub fn ui(&self) -> &engine_ui::Ui {
        &self.runtime.ui
    }

    /// Every UI node clicked this frame — by the pointer, or by a
    /// keyboard/gamepad activation of the focused node
    /// ([`GameConfig::ui_navigation`]).
    ///
    /// Computed before [`Game::update`] runs, so a game reads this
    /// frame's clicks, not last frame's. Both input routes land in the
    /// same list on purpose: a menu handler should never need to know
    /// which device pressed the button.
    pub fn ui_clicked(&self) -> &[engine_ui::NodeId] {
        &self.runtime.ui_clicked
    }

    /// Whether `id` was clicked this frame. See [`GameContext::ui_clicked`].
    pub fn was_clicked(&self, id: engine_ui::NodeId) -> bool {
        self.runtime.ui_clicked.contains(&id)
    }

    /// The audio backend, or `None` if it failed to start.
    ///
    /// Almost nothing needs this: spawn an `engine_ecs::AudioEmitter` and
    /// the frame loop plays it. Reach for it to change master volume, or
    /// to play a one-off non-spatial sound directly.
    pub fn audio_mut(&mut self) -> Option<&mut engine_audio::AudioContext> {
        self.runtime.audio.as_mut()
    }

    /// Whether audio is available. `false` means the machine had no
    /// usable output device and the game is running silent.
    pub fn has_audio(&self) -> bool {
        self.runtime.audio.is_some()
    }

    /// Adds a mixer bus at `volume_decibels` (`0.0` = unity) and returns
    /// its index, for `engine_ecs::AudioEmitter::on_bus`.
    ///
    /// Buses give a group of sounds one shared volume — music separate
    /// from effects, say. A bussed emitter plays flat rather than
    /// spatially; see the `engine_ecs::audio` module docs.
    ///
    /// Returns `None` if audio is unavailable or the backend refused
    /// another track.
    pub fn add_bus(&mut self, volume_decibels: f32) -> Option<usize> {
        let audio = self.runtime.audio.as_mut()?;
        match audio.add_bus(volume_decibels) {
            Ok(bus) => {
                self.runtime.buses.push(bus);
                Some(self.runtime.buses.len() - 1)
            }
            Err(err) => {
                tracing::warn!(error = %err, "could not create an audio bus");
                None
            }
        }
    }

    /// Sets the volume of a bus previously returned by
    /// [`GameContext::add_bus`]. Unknown indices are ignored.
    pub fn set_bus_volume(&mut self, bus: usize, volume_decibels: f32) {
        if let Some(bus) = self.runtime.buses.get_mut(bus) {
            bus.set_volume(volume_decibels, engine_audio::Tween::default());
        }
    }

    /// The physics world, for raycasts and reading body state.
    ///
    /// `engine_physics::cast_ray(ctx.physics(), origin, dir, max, None)`
    /// is the usual reason to reach for this.
    pub fn physics(&self) -> &engine_physics::PhysicsWorld {
        &self.runtime.physics
    }

    /// Mutable access to the physics world: apply forces, set velocities,
    /// add or remove bodies mid-game.
    ///
    /// The frame loop steps this automatically once per fixed update —
    /// don't call [`engine_physics::PhysicsWorld::step`] yourself, or the
    /// simulation advances twice per tick.
    pub fn physics_mut(&mut self) -> &mut engine_physics::PhysicsWorld {
        &mut self.runtime.physics
    }

    /// Spawns an entity with a simulated rigid body: inserts the body and
    /// collider into the physics world and gives the entity the matching
    /// `RigidBody`, `Collider`, and `Transform` components.
    ///
    /// The `Transform` starts at the body's own initial pose and is
    /// overwritten from the simulation after every fixed step, so a
    /// dynamic body drives its entity without the game touching
    /// `engine_physics` at all.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use engine::prelude::*;
    /// # fn demo(ctx: &mut GameContext<'_>) {
    /// ctx.spawn_body(
    ///     RigidBodyBuilder::dynamic().translation(glam::Vec3::new(0.0, 5.0, 0.0)),
    ///     ColliderBuilder::cuboid(0.5, 0.5, 0.5),
    /// );
    /// # }
    /// ```
    pub fn spawn_body(
        &mut self,
        rigid_body: engine_physics::RigidBodyBuilder,
        collider: engine_physics::ColliderBuilder,
    ) -> Entity {
        let (body, collider) = engine_ecs::components::RigidBody::spawn(
            &mut self.runtime.physics,
            rigid_body,
            collider,
        );
        let pose = self
            .runtime
            .physics
            .rapier
            .bodies
            .get(body.0)
            .map(|b| engine_utils::Transform {
                translation: b.translation(),
                rotation: *b.rotation(),
                scale: glam::Vec3::ONE,
            })
            .unwrap_or(engine_utils::Transform::IDENTITY);
        self.runtime
            .ecs
            .world_mut()
            .spawn((
                body,
                collider,
                engine_ecs::components::Transform::from(pose),
            ))
            .id()
    }

    /// Shared access to the ECS world, for reading entity state.
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
        // Read before `setup` so the snapshot is available to it, but
        // hand the game's own data over *after*, so a game applies saved
        // state to a world it has finished building.
        let saved_data = runtime.save_path.clone().and_then(|path| {
            let save = load_save(&path)?;
            runtime.loaded_world = Some(save.world);
            Some(save.game)
        });

        if let Err(err) = self.game.setup(&mut GameContext {
            runtime: &mut runtime,
        }) {
            self.fail(err);
            return;
        }
        if let Some(data) = saved_data
            && !data.is_empty()
        {
            self.game.load_data(&data);
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
                    runtime.ui.set_screen(width as f32, height as f32);
                    runtime.ui_pipeline.resize(&runtime.gpu, width, height);
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
                let raw_frame_seconds = self
                    .last_frame
                    .map(|last| now.duration_since(last).as_secs_f32())
                    .unwrap_or(0.0);
                self.last_frame = Some(now);
                let frame_seconds = clamp_frame_seconds(raw_frame_seconds);

                // Gamepads polled before the game's hooks so a button
                // pressed this frame is visible to them, and before
                // `end_frame` clears the edges.
                let gamepad = runtime.gamepads.poll();
                runtime.input.apply_gamepad(&gamepad);

                runtime.time.advance(frame_seconds);

                let steps = runtime.ticker.advance(frame_seconds);
                let step_seconds = runtime.ticker.step_seconds();
                // Per fixed step, in this order: the game applies
                // forces, physics integrates them, then simulated poses
                // land in `Transform` so `update` and rendering see the
                // current state. Stepping once per *frame* instead would
                // make the simulation depend on frame rate.
                for _ in 0..steps {
                    self.game.fixed_update(
                        &mut GameContext {
                            runtime: &mut *runtime,
                        },
                        step_seconds,
                    );
                    step_physics(runtime, step_seconds);
                }
                // UI laid out and hit-tested before `update`, so a game
                // reading "was this button clicked" sees this frame's
                // click rather than last frame's.
                let pointer = engine_ui::PointerInput {
                    position: runtime
                        .input
                        .cursor_position()
                        .map(|(x, y)| [x as f32, y as f32])
                        .unwrap_or([f32::MIN, f32::MIN]),
                    pressed: runtime
                        .input
                        .is_mouse_button_pressed(engine_platform::MouseButton::Left),
                    released: runtime
                        .input
                        .is_mouse_button_released(engine_platform::MouseButton::Left),
                };
                runtime.ui.layout();
                let mut clicked = runtime.ui.interact(pointer);

                // Menu navigation after the pointer pass, so a click this
                // frame has already moved focus and an activation lands
                // on the node the player just touched.
                if runtime.ui_navigation {
                    let (direction, activate) =
                        ui_navigation_input(&runtime.input, &mut runtime.ui_nav_stick);
                    if let Some(direction) = direction {
                        runtime.ui.focus_direction(direction);
                    }
                    if activate
                        && let Some(id) = runtime.ui.activate()
                        && !clicked.contains(&id)
                    {
                        clicked.push(id);
                    }
                }
                runtime.ui_clicked = clicked;

                self.game.update(
                    &mut GameContext {
                        runtime: &mut *runtime,
                    },
                    frame_seconds,
                );
                runtime.input.end_frame();

                // Laid out a second time, because `update` may have built
                // or restyled nodes. Without this a menu opened this frame
                // draws at zero size — invisible for one frame, and
                // unnavigable, since focus is decided from laid-out
                // rectangles. A tree walk over a handful of nodes; cheaper
                // than the class of bug it removes.
                runtime.ui.layout();

                // After the game's hooks (so a sound triggered this frame
                // starts this frame) and before rendering, which runs
                // transform propagation of its own.
                // Animation before propagation: a pose written here is
                // what this frame's transforms and rendering see.
                engine_ecs::update_animations(runtime.ecs.world_mut(), &runtime.gpu, frame_seconds);
                engine_ecs::propagate_transforms(runtime.ecs.world_mut());
                update_audio(runtime);
                update_shadow_fit(runtime);

                render_frame(runtime, frame_seconds);

                // Autosave after the frame, so a save captures a
                // fully-updated world rather than a half-stepped one.
                runtime.since_save += frame_seconds;
                let due = runtime
                    .save_policy
                    .interval()
                    .is_some_and(|interval| runtime.since_save >= interval);
                if std::mem::take(&mut runtime.save_requested) || due {
                    runtime.since_save = 0.0;
                    persist(runtime, &self.game);
                }
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

    fn on_exit(&mut self) {
        // Last chance to persist. Without this, everything since the last
        // autosave tick is lost on a clean quit — which is most quits.
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        if runtime.save_policy.saves_on_exit() {
            persist(runtime, &self.game);
        }
    }
}

/// Writes the current world and the game's own data to the save path.
///
/// Never fatal: a save that cannot be written is logged and the game
/// keeps running. Losing a frame of progress is bad; crashing the player
/// out to lose all of it is worse.
fn persist<G: Game>(runtime: &mut Runtime, game: &G) {
    let Some(path) = runtime.save_path.clone() else {
        return;
    };
    let data = game.save_data().unwrap_or_default();
    let save = engine_scene::SaveGame::capture(runtime.ecs.world_mut(), data);
    match save.save_to_file(&path) {
        Ok(()) => tracing::debug!(path = %path.display(), "saved"),
        Err(err) => tracing::warn!(error = %err, path = %path.display(), "could not save"),
    }
}

/// Reads the save at `path`, if one is there.
///
/// A missing file is the normal first-run case, not an error. A corrupt
/// or newer-than-supported file *is* reported, then ignored — starting
/// fresh beats refusing to launch.
fn load_save(path: &std::path::Path) -> Option<engine_scene::SaveGame<String>> {
    if !path.is_file() {
        return None;
    }
    match engine_scene::SaveGame::<String>::load_from_file(path) {
        Ok(save) => {
            tracing::info!(path = %path.display(), "loaded save");
            Some(save)
        }
        Err(err) => {
            tracing::warn!(error = %err, path = %path.display(), "ignoring unreadable save");
            None
        }
    }
}

/// One fixed physics step: integrate, then write the simulated poses
/// back into the ECS.
///
/// Extracted from the frame loop so the step/sync pairing is testable —
/// the loop itself lives inside a `PlatformHandler` that needs a real
/// window.
///
/// A rejected timestep is logged and skipped rather than propagated:
/// `step_seconds` comes from [`FixedTimestep`] and is always finite and
/// positive, so this is a guard against future callers, not an expected
/// path. Never panics.
fn step_physics(runtime: &mut Runtime, step_seconds: f32) {
    if let Err(err) = runtime.physics.step(step_seconds) {
        tracing::error!(error = %err, "physics step rejected; simulation did not advance");
        return;
    }
    engine_ecs::sync_rigid_bodies(runtime.ecs.world_mut(), &runtime.physics);
}

/// Extends the glyph atlas with one opaque white row, returning the
/// combined RGBA8 image and the UV of a white texel inside it.
///
/// Solid-colour UI quads need *something* to sample. Giving them a white
/// texel in the same atlas means fills, images and glyphs all go through
/// one pipeline and one bind group, instead of a second pipeline whose
/// only difference is "no texture".
fn ui_atlas_with_white(atlas: &engine_renderer::GlyphAtlas) -> (u32, u32, Vec<u8>, [f32; 4]) {
    let (width, glyph_height) = atlas.size();
    let height = glyph_height + 1;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    pixels.extend_from_slice(atlas.pixels());
    // The extra row: fully opaque white.
    pixels.extend(std::iter::repeat_n(255u8, (width * 4) as usize));

    // Sample the middle of that row, away from any filtering bleed at the
    // boundary with the glyph rows above.
    let v = (glyph_height as f32 + 0.5) / height as f32;
    let u = 0.5 / width as f32;
    (width, height, pixels, [u, v, u, v])
}

/// Turns the UI tree's draw list into instanced quads.
///
/// Text expands here rather than in `engine_ui`, which deliberately knows
/// nothing about fonts — it emits a string and a rect, and this is where
/// that becomes glyph quads.
/// Deadzone the left stick must clear before it counts as a menu
/// direction. Higher than the gameplay deadzone: a stick resting just
/// off centre must not walk the focus across a menu.
const UI_STICK_THRESHOLD: f32 = 0.6;

/// The stick's discrete direction this frame, reported **only on the
/// frame it changes** — a held stick moves focus once, not once per
/// frame. `previous` carries the last direction between calls.
///
/// The dominant axis wins, so a diagonal push picks one direction rather
/// than jumping twice.
fn stick_nav_edge(x: f32, y: f32, previous: &mut (i8, i8)) -> Option<engine_ui::FocusDirection> {
    let discrete = |v: f32| {
        if v >= UI_STICK_THRESHOLD {
            1
        } else if v <= -UI_STICK_THRESHOLD {
            -1
        } else {
            0
        }
    };
    let current = if x.abs() >= y.abs() {
        (discrete(x), 0)
    } else {
        (0, discrete(y))
    };

    let changed = current != *previous;
    *previous = current;
    if !changed {
        return None;
    }
    match current {
        (1, _) => Some(engine_ui::FocusDirection::Right),
        (-1, _) => Some(engine_ui::FocusDirection::Left),
        // Stick Y is positive up; screen Y grows downwards.
        (_, 1) => Some(engine_ui::FocusDirection::Up),
        (_, -1) => Some(engine_ui::FocusDirection::Down),
        _ => None,
    }
}

/// This frame's menu navigation: at most one direction (so a diagonal
/// press does not move twice) plus whether the player asked to activate.
fn ui_navigation_input(
    input: &engine_platform::InputState,
    previous_stick: &mut (i8, i8),
) -> (Option<engine_ui::FocusDirection>, bool) {
    use engine_platform::{GamepadAxis, GamepadButton, KeyCode};
    use engine_ui::FocusDirection;

    let pressed = [
        (KeyCode::ArrowUp, GamepadButton::DPadUp, FocusDirection::Up),
        (
            KeyCode::ArrowDown,
            GamepadButton::DPadDown,
            FocusDirection::Down,
        ),
        (
            KeyCode::ArrowLeft,
            GamepadButton::DPadLeft,
            FocusDirection::Left,
        ),
        (
            KeyCode::ArrowRight,
            GamepadButton::DPadRight,
            FocusDirection::Right,
        ),
    ]
    .into_iter()
    .find_map(|(key, button, direction)| {
        (input.is_key_pressed(key) || input.is_button_pressed(button)).then_some(direction)
    });

    let direction = pressed.or_else(|| {
        stick_nav_edge(
            input.axis(GamepadAxis::LeftStickX),
            input.axis(GamepadAxis::LeftStickY),
            previous_stick,
        )
    });

    let activate = input.is_key_pressed(KeyCode::Enter)
        || input.is_key_pressed(KeyCode::Space)
        || input.is_button_pressed(GamepadButton::South);

    (direction, activate)
}

fn ui_quads(
    commands: &[engine_ui::DrawCommand],
    atlas: &engine_renderer::GlyphAtlas,
    white_uv: [f32; 4],
) -> Vec<engine_renderer::UiQuad> {
    /// Text height in pixels. Fixed for now — `engine_ui` has no
    /// per-node font size, so inventing one here would be a lie.
    const TEXT_PIXEL_HEIGHT: f32 = 16.0;

    let mut quads = Vec::new();
    for command in commands {
        let rect = command.rect;
        match &command.kind {
            engine_ui::DrawKind::Fill(color) => quads.push(engine_renderer::UiQuad {
                rect: [rect.x, rect.y, rect.w, rect.h],
                uv: white_uv,
                tint: [color.r, color.g, color.b, color.a],
            }),
            engine_ui::DrawKind::Image { tint, .. } => {
                // No texture store to resolve an `AssetId` against yet, so
                // an image draws as its tint rather than vanishing. Wiring
                // real image widgets needs a texture registry (see the
                // `DrawKind::Image` note in the task write-up).
                quads.push(engine_renderer::UiQuad {
                    rect: [rect.x, rect.y, rect.w, rect.h],
                    uv: white_uv,
                    tint: [tint.r, tint.g, tint.b, tint.a],
                });
            }
            engine_ui::DrawKind::Text { text, color } => {
                let layout = engine_renderer::layout_text(
                    text,
                    TEXT_PIXEL_HEIGHT,
                    Some(rect.w.max(TEXT_PIXEL_HEIGHT)),
                );
                for glyph in &layout.glyphs {
                    let uv = atlas.uv(glyph.character);
                    quads.push(engine_renderer::UiQuad {
                        rect: [
                            rect.x + glyph.x,
                            rect.y + glyph.y,
                            glyph.width,
                            glyph.height,
                        ],
                        uv: [uv.min[0], uv.min[1], uv.max[0], uv.max[1]],
                        tint: [color.r, color.g, color.b, color.a],
                    });
                }
            }
        }
    }
    quads
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

    let light_space_matrix =
        directional_light_view_projection(sun_direction, camera.target, SHADOW_RADIUS);
    let shadow_map = gpu.create_shadow_map(&shadow_pipeline, light_space_matrix);

    let camera_binding = gpu.create_camera_binding(&pipeline, &camera.to_uniform());
    let lights_binding = gpu.create_lights_binding(&pipeline, &lights.to_uniform(), &shadow_map);
    let skybox = gpu.create_skybox_binding(
        &skybox_pipeline,
        &skybox_uniform(&camera, sun_direction, sun_color),
    );

    let hdr_target = gpu.create_hdr_target(width, height);
    let post = gpu.create_post_process_stack(&hdr_target, width, height, config.post);

    let sprite_pipeline = gpu.create_sprite_pipeline("sprites", &pipeline);
    let (quad_vertices, quad_indices) = engine_renderer::quad();
    let sprite_quad = gpu
        .create_mesh("sprite quad", &quad_vertices, &quad_indices)
        .map_err(|err| GameError::InvalidConfig(err.to_string()))?;

    let particle_pipelines = gpu.create_particle_pipelines("particles");
    let particle_camera = gpu.create_particle_camera_binding(&particle_pipelines, &camera);
    let vegetation_pipeline = gpu.create_vegetation_pipeline(&pipeline, "vegetation");
    let wind = Wind::BREEZE;
    let wind_binding = gpu.create_wind_binding(&vegetation_pipeline, &wind, 0.0);

    // The UI atlas is the glyph sheet with one opaque white texel
    // appended, so solid fills and text share a single pipeline and
    // binding.
    let glyph_atlas = engine_renderer::GlyphAtlas::build();
    let (atlas_width, atlas_height, atlas_pixels, ui_white_uv) = ui_atlas_with_white(&glyph_atlas);
    let atlas_texture =
        gpu.create_texture_from_rgba("ui atlas", atlas_width, atlas_height, &atlas_pixels);
    let ui_pipeline = gpu.create_ui_pipeline("ui", &atlas_texture, ui_white_uv);

    let ticker = FixedTimestep::from_hz(config.fixed_hz)
        .map_err(|err| GameError::InvalidConfig(err.to_string()))?;

    Ok(Runtime {
        gpu,
        ecs: Ecs::new(),
        physics: engine_physics::PhysicsWorld::new(config.gravity),
        // A machine with no sound device must still run the game.
        audio: match engine_audio::AudioContext::new() {
            Ok(audio) => Some(audio),
            Err(err) => {
                tracing::warn!(error = %err, "audio unavailable; running silent");
                None
            }
        },
        buses: Vec::new(),
        ui: engine_ui::Ui::new(width as f32, height as f32),
        glyph_atlas,
        ui_pipeline,
        ui_white_uv,
        ui_clicked: Vec::new(),
        ui_navigation: config.ui_navigation,
        ui_nav_stick: (0, 0),
        save_path: config.save_path.clone(),
        save_policy: config.save_policy,
        since_save: 0.0,
        save_requested: false,
        loaded_world: None,
        assets: RenderAssets::new(),
        input: engine_platform::InputState::new(),
        gamepads: engine_platform::Gamepads::new(),
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
        sprite_pipeline,
        sprite_quad,
        sprite_atlas: None,
        warned_missing_atlas: false,
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
/// How far around the camera's focus the shadow map covers, in world
/// units.
///
/// One map fitted to the camera rather than the world origin: before
/// this, shadows were fitted to a fixed 8-unit ball at `Vec3::ZERO`, so
/// walking away from the origin lost them entirely. Cascades would give
/// crisp near shadows *and* long range; this is the single-map 80%.
const SHADOW_RADIUS: f32 = 40.0;

/// Re-fits the shadow projection around wherever the camera is looking.
///
/// Cheap — one 4x4 matrix and a uniform write per frame — and it is what
/// keeps shadows present as the player moves.
fn update_shadow_fit(runtime: &mut Runtime) {
    let (sun_direction, _) = runtime.sun();
    let light_space =
        directional_light_view_projection(sun_direction, runtime.camera.target, SHADOW_RADIUS);
    runtime.gpu.write_uniform_buffer(
        &runtime.shadow_map.buffer,
        &engine_renderer::ShadowUniform::from(light_space),
    );
}

fn update_audio(runtime: &mut Runtime) {
    let Some(audio) = runtime.audio.as_mut() else {
        return;
    };
    engine_ecs::update_audio(runtime.ecs.world_mut(), audio, &mut runtime.buses);
}

fn render_frame(runtime: &mut Runtime, dt: f32) {
    // Built before the borrow-heavy render call, since it reads the UI
    // tree while that call takes the world mutably.
    let ui_quads = ui_quads(
        &runtime.ui.draw_list(
            runtime
                .input
                .cursor_position()
                .map(|(x, y)| [x as f32, y as f32])
                .unwrap_or([f32::MIN, f32::MIN]),
        ),
        &runtime.glyph_atlas,
        runtime.ui_white_uv,
    );

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

    // Sprites need an atlas the game supplies; without one there is
    // nothing to sample, so the draw is skipped. Warn once — a game that
    // spawned sprites and forgot the atlas would otherwise just see an
    // empty screen with no explanation.
    let sprites = match &runtime.sprite_atlas {
        Some(atlas) => Some(engine_ecs::SpriteTarget {
            pipeline: &runtime.sprite_pipeline,
            quad: &runtime.sprite_quad,
            atlas,
        }),
        None => {
            if !runtime.warned_missing_atlas
                && runtime
                    .ecs
                    .world_mut()
                    .query::<&engine_ecs::components::Sprite>()
                    .iter(runtime.ecs.world())
                    .next()
                    .is_some()
            {
                runtime.warned_missing_atlas = true;
                tracing::warn!(
                    "entities have a Sprite but no sprite atlas is set; \
                     call GameContext::set_sprite_atlas to draw them"
                );
            }
            None
        }
    };

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
        sprites,
        particles,
        Some((&runtime.vegetation_pipeline, &runtime.wind_binding)),
        (!ui_quads.is_empty()).then_some((&runtime.ui_pipeline, ui_quads.as_slice())),
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
    fn a_held_stick_moves_focus_once() {
        let mut previous = (0, 0);
        assert_eq!(
            stick_nav_edge(0.0, -1.0, &mut previous),
            Some(engine_ui::FocusDirection::Down),
            "stick Y is positive up, screen Y grows down",
        );
        assert_eq!(
            stick_nav_edge(0.0, -1.0, &mut previous),
            None,
            "holding it must not scroll the menu every frame",
        );
        // Back to centre, then pushed again: that is a new press.
        assert_eq!(stick_nav_edge(0.0, 0.0, &mut previous), None);
        assert_eq!(
            stick_nav_edge(0.0, -1.0, &mut previous),
            Some(engine_ui::FocusDirection::Down),
        );
    }

    #[test]
    fn a_resting_stick_is_not_a_direction() {
        let mut previous = (0, 0);
        assert_eq!(stick_nav_edge(0.4, -0.3, &mut previous), None);
        assert_eq!(previous, (0, 0));
    }

    #[test]
    fn a_diagonal_push_picks_one_direction() {
        let mut previous = (0, 0);
        // Slightly more horizontal than vertical.
        assert_eq!(
            stick_nav_edge(0.9, 0.8, &mut previous),
            Some(engine_ui::FocusDirection::Right),
        );
    }

    #[test]
    fn ui_navigation_is_on_by_default() {
        assert!(GameConfig::new("Test", 800, 400).ui_navigation);
    }

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
        time.advance(0.05);
        time.advance(0.025);
        assert!((time.elapsed - 0.075).abs() < 1e-6);
        assert!((time.delta - 0.025).abs() < 1e-6);
        assert_eq!(time.frame, 2);

        time.advance(f32::NAN);
        time.advance(-1.0);
        time.advance(f32::INFINITY);
        assert!((time.elapsed - 0.075).abs() < 1e-6);
        assert_eq!(time.delta, 0.0);
        assert_eq!(time.frame, 5);
    }

    #[test]
    fn variable_delta_is_bounded_before_it_reaches_gameplay() {
        let mut time = Time::default();
        time.advance(MAX_FRAME_SECONDS * 4.0);
        assert_eq!(time.delta, MAX_FRAME_SECONDS);
        assert_eq!(time.elapsed, MAX_FRAME_SECONDS);

        time.advance(0.016);
        assert!((time.elapsed - (MAX_FRAME_SECONDS + 0.016)).abs() < 1e-6);
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

    // --- T-05: physics in the fixed-step loop ---------------------

    use engine_ecs::components::{RigidBody, Transform as EcsTransform};
    use engine_ecs::prelude::World;
    use engine_physics::{ColliderBuilder, PhysicsWorld, RigidBodyBuilder};

    /// A dynamic ball at `y = 10` over nothing, plus the world holding it.
    fn falling_ball() -> (PhysicsWorld, World, engine_ecs::prelude::Entity) {
        let mut physics = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));
        let (body, collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 10.0, 0.0)),
            ColliderBuilder::ball(0.5),
        );
        let mut world = World::new();
        let entity = world.spawn((body, collider, EcsTransform::default())).id();
        (physics, world, entity)
    }

    fn height_of(world: &World, entity: engine_ecs::prelude::Entity) -> f32 {
        world
            .get::<EcsTransform>(entity)
            .map(|t| t.0.translation.y)
            .unwrap_or(f32::NAN)
    }

    /// The exact pairing the frame loop performs per fixed step.
    fn step_once(physics: &mut PhysicsWorld, world: &mut World, step: f32) {
        physics.step(step).expect("a positive finite step is valid");
        engine_ecs::sync_rigid_bodies(world, physics);
    }

    #[test]
    fn no_fixed_step_means_no_physics_step() {
        let (physics, mut world, entity) = falling_ball();
        // Sync without stepping: the body is still at its spawn pose.
        engine_ecs::sync_rigid_bodies(&mut world, &physics);
        assert!(
            (height_of(&world, entity) - 10.0).abs() < 1e-3,
            "a frame that produced zero fixed steps must not advance the simulation",
        );
    }

    #[test]
    fn each_fixed_step_advances_the_simulation_once() {
        let (mut physics, mut world, entity) = falling_ball();
        let step = 1.0 / 60.0;

        step_once(&mut physics, &mut world, step);
        let after_one = height_of(&world, entity);
        assert!(after_one < 10.0, "one step must move a falling body");

        for _ in 0..9 {
            step_once(&mut physics, &mut world, step);
        }
        let after_ten = height_of(&world, entity);
        assert!(
            after_ten < after_one,
            "ten steps must fall further than one",
        );
    }

    #[test]
    fn sync_writes_body_pose_into_transform() {
        let (mut physics, mut world, entity) = falling_ball();
        for _ in 0..30 {
            step_once(&mut physics, &mut world, 1.0 / 60.0);
        }
        let body_y = world
            .get::<RigidBody>(entity)
            .and_then(|rb| physics.rapier.bodies.get(rb.0))
            .map(|b| b.translation().y)
            .expect("body should still exist");
        assert!(
            (height_of(&world, entity) - body_y).abs() < 1e-5,
            "Transform must mirror the simulated pose exactly",
        );
    }

    #[test]
    fn a_dynamic_body_comes_to_rest_on_a_static_floor() {
        let mut physics = PhysicsWorld::new(Vec3::new(0.0, -9.81, 0.0));
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, 0.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.5, 10.0),
        );
        let (body, collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 5.0, 0.0)),
            ColliderBuilder::cuboid(0.5, 0.5, 0.5),
        );
        let mut world = World::new();
        let entity = world.spawn((body, collider, EcsTransform::default())).id();

        // Two seconds at 60 Hz is ample for a 4.5-unit drop to settle.
        for _ in 0..120 {
            step_once(&mut physics, &mut world, 1.0 / 60.0);
        }

        let resting = height_of(&world, entity);
        assert!(
            resting > 0.5 && resting < 1.5,
            "box should rest on the floor surface, got y = {resting}",
        );
    }

    #[test]
    fn config_rejects_non_finite_gravity() {
        let mut config = GameConfig::new("Test", 800, 600);
        config.gravity = Vec3::new(0.0, f32::NAN, 0.0);
        assert!(config.validate().is_err());
        config.gravity = Vec3::new(f32::INFINITY, 0.0, 0.0);
        assert!(config.validate().is_err());
        config.gravity = Vec3::ZERO;
        assert!(config.validate().is_ok(), "zero-g is a valid game");
    }

    #[test]
    fn default_gravity_points_down() {
        let config = GameConfig::new("Test", 800, 600);
        assert!(config.gravity.y < 0.0);
        assert!(config.validate().is_ok());
    }
}
