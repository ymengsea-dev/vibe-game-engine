//! Validation Game 2 — Stage 2's second (and final) validation game: a
//! small platformer.
//!
//! Move right with A/D or the arrow keys, press Space to jump over the
//! spikes, then clear the pit to reach the goal on the far ledge. Exercises
//! `engine_physics::move_character_2d` — this iteration's actual engine
//! deliverable, a reusable 2D character controller (the 2D counterpart to
//! `engine_physics::move_character`'s rapier3d-backed one) — plus every
//! Stage 2 piece Validation Game 1 already proved (sprite renderer,
//! orthographic camera, action-mapping, scene save/load), reused here
//! unmodified rather than re-proven.
//!
//! Run with `cargo run -p game --example validation_game_2`.
//!
//! Deliberately out of scope (matches Validation Game 1's stated limits):
//! no on-screen win/lose text (`tracing::info!` instead), no
//! `AssetDatabase` integration, no wall-jump/coyote-time/other platformer
//! feel polish — this validates the collision-resolution mechanics, not
//! game feel.

use std::sync::Arc;

use engine::ecs::components as ecs_components;
use engine::ecs::prelude::{Component, Resource, With, World};
use engine::prelude::*;
use glam::Vec3Swizzles;

/// Orthographic camera visible height, in world units.
const CAMERA_HEIGHT: f32 = 10.0;
/// Horizontal run speed, world units/second.
const MOVE_SPEED: f32 = 5.0;
/// Downward acceleration, world units/second².
const GRAVITY: f32 = -25.0;
/// Upward speed applied on jump, world units/second.
const JUMP_VELOCITY: f32 = 11.0;
/// Falling below this Y ends the game — missed the pit jump.
const FALL_THRESHOLD: f32 = -10.0;

const PLAYER_HALF_EXTENTS: glam::Vec2 = glam::Vec2::new(0.3, 0.4);

/// Ground on the start side of the pit: spans x = -9..-2, top surface at
/// y = -3.0.
const GROUND_A: Aabb2d = Aabb2d {
    center: glam::Vec2::new(-5.5, -3.5),
    half_extents: glam::Vec2::new(3.5, 0.5),
};
/// Ground on the goal side of the pit: spans x = 2..9, same height as
/// [`GROUND_A`] — the gap between them (width 4) is the pit; clearing it
/// needs one running jump (air time `2 * JUMP_VELOCITY / -GRAVITY` ≈
/// 0.88s, times [`MOVE_SPEED`] ≈ 4.4 world units of horizontal range,
/// comfortably more than the gap).
const GROUND_B: Aabb2d = Aabb2d {
    center: glam::Vec2::new(5.5, -3.5),
    half_extents: glam::Vec2::new(3.5, 0.5),
};
const PLATFORMS: [Aabb2d; 2] = [GROUND_A, GROUND_B];

/// Spikes on [`GROUND_A`], sitting on its top surface — touch them and
/// it's [`Phase::Lost`].
const HAZARD: Aabb2d = Aabb2d {
    center: glam::Vec2::new(-6.0, -2.7),
    half_extents: glam::Vec2::new(0.4, 0.3),
};
/// The goal marker on [`GROUND_B`] — reach it and it's [`Phase::Won`].
const GOAL: Aabb2d = Aabb2d {
    center: glam::Vec2::new(5.0, -2.7),
    half_extents: glam::Vec2::new(0.3, 0.3),
};

const PLAYER_START: glam::Vec2 = glam::Vec2::new(-8.0, -2.6);

/// Atlas cell size, in pixels, and the grid region each role samples
/// ([`AtlasLayout::add_grid`]'s `"{col}_{row}"` convention).
const CELL_SIZE: u32 = 32;
const PLAYER_REGION: &str = "0_0";
const GROUND_REGION: &str = "1_0";
const HAZARD_REGION: &str = "2_0";
const GOAL_REGION: &str = "3_0";

const PLAYER_COLOR: [u8; 4] = [80, 140, 255, 255];
const GROUND_COLOR: [u8; 4] = [120, 90, 70, 255];
const HAZARD_COLOR: [u8; 4] = [230, 60, 60, 255];
const GOAL_COLOR: [u8; 4] = [80, 220, 100, 255];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// The player-controlled entity. Marker only — game-local, not part of
/// `engine_ecs::components`.
#[derive(Component)]
struct Player;

/// How the game is currently going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Playing,
    Won,
    Lost,
}

/// Win/lose state, plus the player's accumulated vertical velocity and
/// last-known grounded state — an ECS resource so it's readable/writable
/// from the same `&mut World` everything else here already has. Vertical
/// velocity/grounded live here rather than as a physics-engine concept:
/// `move_character_2d` "has no gravity or input policy of its own, only
/// collision-aware movement resolution" (its own docs) — this game
/// supplies both, the same split `game/src/main.rs`'s 3D demo already
/// uses for its `move_character`-driven player.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
struct GameState {
    phase: Phase,
    vertical_velocity: f32,
    grounded: bool,
}

/// The actions this game's player can perform, bound to raw keys via
/// [`ActionMap`] — gameplay code below never names a [`KeyCode`] directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PlayerAction {
    Left,
    Right,
    Jump,
}

/// Builds a `colors.len() * cell_size` wide, `cell_size` tall RGBA8 image:
/// one solid-color square per entry in `colors`, left to right. Same
/// procedural stand-in Validation Game 1 and `game/src/main.rs`'s 3D demo
/// both already use, rather than a binary asset file in the repo.
fn atlas_rgba(cell_size: u32, colors: &[[u8; 4]]) -> Vec<u8> {
    let width = cell_size * colors.len() as u32;
    let mut pixels = Vec::with_capacity((width * cell_size * 4) as usize);
    for _row in 0..cell_size {
        for color in colors {
            for _col in 0..cell_size {
                pixels.extend_from_slice(color);
            }
        }
    }
    pixels
}

/// Builds this game's level layout as serializable scene data — not yet
/// spawned into any ECS world, just the data that gets saved to disk.
fn build_level(atlas_id: AssetId) -> Scene {
    let atlas_ref = AssetRef {
        id: atlas_id.to_string(),
    };
    let sprite = |region: &str, aabb: &Aabb2d, color: [f32; 4]| SpriteData {
        atlas: atlas_ref.clone(),
        region: region.to_string(),
        size: (aabb.half_extents * 2.0).to_array(),
        color,
    };
    let entity_at = |name: &str, position: glam::Vec2, sprite_data: SpriteData| SceneEntity {
        name: Some(name.to_string()),
        transform: Some(TransformData::from(Transform::from_translation(
            position.extend(0.0),
        ))),
        sprite: Some(sprite_data),
        ..Default::default()
    };

    let player_aabb = Aabb2d {
        center: PLAYER_START,
        half_extents: PLAYER_HALF_EXTENTS,
    };

    Scene {
        entities: vec![
            entity_at(
                "Player",
                PLAYER_START,
                sprite(PLAYER_REGION, &player_aabb, WHITE),
            ),
            entity_at(
                "Ground A",
                GROUND_A.center,
                sprite(GROUND_REGION, &GROUND_A, WHITE),
            ),
            entity_at(
                "Ground B",
                GROUND_B.center,
                sprite(GROUND_REGION, &GROUND_B, WHITE),
            ),
            entity_at(
                "Hazard",
                HAZARD.center,
                sprite(HAZARD_REGION, &HAZARD, WHITE),
            ),
            entity_at("Goal", GOAL.center, sprite(GOAL_REGION, &GOAL, WHITE)),
        ],
        ..Default::default()
    }
}

/// Spawns every sprite entity in `scene` into `world`, resolving each
/// [`SpriteData::region`] against `atlas_layout` and tagging [`Player`]
/// from the loaded [`SceneEntity::name`] — classified once here at load
/// time, not by re-matching strings every frame. Ground/hazard/goal need
/// no marker component: their positions are the fixed [`GROUND_A`]/
/// [`GROUND_B`]/[`HAZARD`]/[`GOAL`] constants, not queried back out of the
/// ECS each frame.
fn spawn_from_scene(world: &mut World, scene: &Scene, atlas_layout: &AtlasLayout) {
    for entity in &scene.entities {
        let Some(sprite_data) = &entity.sprite else {
            continue;
        };
        let Some(uv) = atlas_layout.uv_rect(&sprite_data.region) else {
            tracing::warn!(region = %sprite_data.region, "unknown atlas region; skipping entity");
            continue;
        };
        let translation = entity
            .transform
            .map(|transform| glam::Vec3::from_array(transform.translation))
            .unwrap_or(glam::Vec3::ZERO);

        let mut sprite = ecs_components::Sprite::new(glam::Vec2::from_array(sprite_data.size), uv);
        sprite.color = sprite_data.color;

        let mut entity_mut = world.spawn((
            ecs_components::Transform::from(Transform::from_translation(translation)),
            sprite,
        ));

        if let Some(name) = &entity.name {
            entity_mut.insert(ecs_components::Name::new(name.clone()));
            if name == "Player" {
                entity_mut.insert(Player);
            }
        }
    }
}

/// GPU resources, the ECS world, and per-frame state for the running game.
struct Scene2D {
    sprite_pipeline: SpritePipeline,
    quad: Mesh,
    camera: Camera,
    camera_binding: CameraBinding,
    atlas_binding: SpriteAtlasBinding,
    ecs: Ecs,
    action_map: ActionMap<PlayerAction>,
    last_frame: std::time::Instant,
}

struct GameHandler {
    input: InputState,
    gpu: Option<GpuContext>,
    scene: Option<Scene2D>,
}

impl GameHandler {
    fn init_gpu_and_scene(&mut self, window: Arc<Window>) {
        let size = window.inner_size();
        let gpu = match GpuContext::new(window) {
            Ok(gpu) => gpu,
            Err(err) => {
                tracing::error!(error = %err, "failed to initialize GPU context");
                return;
            }
        };

        let sprite_pipeline = gpu.create_sprite_pipeline("sprite");

        let (quad_vertices, quad_indices) = quad();
        let quad_mesh = match gpu.create_mesh("sprite quad", &quad_vertices, &quad_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create sprite quad mesh");
                return;
            }
        };

        let aspect_ratio = size.width as f32 / size.height.max(1) as f32;
        let camera = Camera::new_orthographic(
            glam::Vec3::new(0.0, 0.0, 5.0),
            glam::Vec3::ZERO,
            aspect_ratio,
            CAMERA_HEIGHT,
        );
        let camera_binding =
            gpu.create_sprite_camera_binding(&sprite_pipeline, &camera.to_uniform());

        let atlas_texture = gpu.create_texture_from_rgba(
            "sprite atlas",
            CELL_SIZE * 4,
            CELL_SIZE,
            &atlas_rgba(
                CELL_SIZE,
                &[PLAYER_COLOR, GROUND_COLOR, HAZARD_COLOR, GOAL_COLOR],
            ),
        );
        let mut atlas_layout = match AtlasLayout::new(CELL_SIZE * 4, CELL_SIZE) {
            Ok(layout) => layout,
            Err(err) => {
                tracing::error!(error = %err, "failed to create atlas layout");
                return;
            }
        };
        if let Err(err) = atlas_layout.add_grid(4, 1) {
            tracing::error!(error = %err, "failed to slice atlas into a grid");
            return;
        }
        let atlas = TextureAtlas::new(atlas_texture, atlas_layout);
        let atlas_binding = gpu.create_sprite_atlas_binding(&sprite_pipeline, &atlas);

        // Build the level, save it to a real file, then load it back —
        // same "saved and loaded as a real scene file" round trip
        // Validation Game 1 proved, reused here rather than re-proven.
        let scene_path = std::env::temp_dir().join("vge-validation-game-2-scene.ron");
        let level = build_level(AssetId::new());
        if let Err(err) = level.save_to_file(&scene_path) {
            tracing::error!(error = %err, path = %scene_path.display(), "failed to save scene");
            return;
        }
        tracing::info!(path = %scene_path.display(), "saved validation game scene");

        let loaded_scene = match Scene::load_from_file(&scene_path) {
            Ok(scene) => scene,
            Err(err) => {
                tracing::error!(error = %err, path = %scene_path.display(), "failed to load scene");
                return;
            }
        };
        tracing::info!(
            path = %scene_path.display(),
            entities = loaded_scene.entities.len(),
            "loaded validation game scene"
        );

        let mut ecs = Ecs::new();
        spawn_from_scene(ecs.world_mut(), &loaded_scene, &atlas.layout);
        ecs.insert_resource(GameState {
            phase: Phase::Playing,
            vertical_velocity: 0.0,
            grounded: false,
        });

        let mut action_map: ActionMap<PlayerAction> = ActionMap::new();
        action_map
            .bind(PlayerAction::Left, Binding::Key(KeyCode::KeyA))
            .bind(PlayerAction::Left, Binding::Key(KeyCode::ArrowLeft))
            .bind(PlayerAction::Right, Binding::Key(KeyCode::KeyD))
            .bind(PlayerAction::Right, Binding::Key(KeyCode::ArrowRight))
            .bind(PlayerAction::Jump, Binding::Key(KeyCode::Space));

        self.scene = Some(Scene2D {
            sprite_pipeline,
            quad: quad_mesh,
            camera,
            camera_binding,
            atlas_binding,
            ecs,
            action_map,
            last_frame: std::time::Instant::now(),
        });
        self.gpu = Some(gpu);
    }
}

impl PlatformHandler for GameHandler {
    fn on_window_ready(&mut self, window: Arc<Window>) {
        tracing::info!("window ready");
        self.init_gpu_and_scene(window);
    }

    fn on_event(&mut self, event: PlatformEvent) {
        self.input.apply_event(&event);
        match event {
            PlatformEvent::CloseRequested => tracing::info!("close requested"),
            PlatformEvent::Resized { width, height } => {
                let Some(gpu) = &mut self.gpu else { return };
                if let Err(err) = gpu.resize(width, height) {
                    tracing::warn!(error = %err, "skipped surface resize");
                    return;
                }
                if let Some(scene) = &mut self.scene {
                    scene.camera.aspect_ratio = width as f32 / height.max(1) as f32;
                    gpu.write_uniform_buffer(
                        &scene.camera_binding.buffer,
                        &scene.camera.to_uniform(),
                    );
                }
            }
            PlatformEvent::RedrawRequested => {
                let (Some(gpu), Some(scene)) = (&self.gpu, &mut self.scene) else {
                    return;
                };

                let now = std::time::Instant::now();
                let dt = now
                    .duration_since(scene.last_frame)
                    .as_secs_f32()
                    .min(1.0 / 20.0);
                scene.last_frame = now;

                let phase = scene
                    .ecs
                    .resource::<GameState>()
                    .map(|state| state.phase)
                    .unwrap_or(Phase::Lost);

                if phase == Phase::Playing {
                    let mut horizontal = 0.0_f32;
                    if scene.action_map.is_held(PlayerAction::Left, &self.input) {
                        horizontal -= 1.0;
                    }
                    if scene.action_map.is_held(PlayerAction::Right, &self.input) {
                        horizontal += 1.0;
                    }
                    let jump_pressed = scene.action_map.is_pressed(PlayerAction::Jump, &self.input);

                    let grounded_before = scene
                        .ecs
                        .resource::<GameState>()
                        .map(|state| state.grounded)
                        .unwrap_or(false);
                    let mut vertical_velocity = scene
                        .ecs
                        .resource::<GameState>()
                        .map(|state| state.vertical_velocity)
                        .unwrap_or(0.0);

                    if jump_pressed && grounded_before {
                        vertical_velocity = JUMP_VELOCITY;
                    }
                    vertical_velocity += GRAVITY * dt;

                    let desired =
                        glam::Vec2::new(horizontal * MOVE_SPEED * dt, vertical_velocity * dt);

                    let mut old_pos = PLAYER_START;
                    {
                        let world = scene.ecs.world_mut();
                        let mut query =
                            world.query_filtered::<&ecs_components::Transform, With<Player>>();
                        if let Ok(transform) = query.single(world) {
                            old_pos = transform.0.translation.xy();
                        }
                    }

                    match move_character_2d(old_pos, PLAYER_HALF_EXTENTS, desired, &PLATFORMS) {
                        Ok(movement) => {
                            let new_pos = old_pos + movement.translation;
                            {
                                let world = scene.ecs.world_mut();
                                let mut query = world
                                    .query_filtered::<&mut ecs_components::Transform, With<Player>>(
                                    );
                                if let Ok(mut transform) = query.single_mut(world) {
                                    transform.0.translation = new_pos.extend(0.0);
                                }
                            }
                            if movement.grounded {
                                vertical_velocity = 0.0;
                            }

                            let player_aabb = Aabb2d {
                                center: new_pos,
                                half_extents: PLAYER_HALF_EXTENTS,
                            };
                            let mut new_phase = None;
                            if aabb_vs_aabb(&player_aabb, &HAZARD) || new_pos.y < FALL_THRESHOLD {
                                new_phase = Some(Phase::Lost);
                            } else if aabb_vs_aabb(&player_aabb, &GOAL) {
                                new_phase = Some(Phase::Won);
                            }

                            if let Some(state) = scene.ecs.resource_mut::<GameState>() {
                                state.vertical_velocity = vertical_velocity;
                                state.grounded = movement.grounded;
                                if let Some(phase) = new_phase {
                                    state.phase = phase;
                                    match phase {
                                        Phase::Won => tracing::info!("you win!"),
                                        Phase::Lost => tracing::info!("you lose!"),
                                        Phase::Playing => {}
                                    }
                                }
                            }
                        }
                        Err(err) => tracing::warn!(error = %err, "skipped character movement"),
                    }
                }

                self.input.end_frame();

                if let Err(err) = extract_and_render_sprites(
                    scene.ecs.world_mut(),
                    gpu,
                    &scene.sprite_pipeline,
                    &scene.quad,
                    &scene.camera_binding,
                    &scene.atlas_binding,
                ) {
                    tracing::error!(error = %err, "frame render failed");
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
    logging::init_default()?;

    let config = EngineConfig::new("VGE Validation Game 2", env!("CARGO_PKG_VERSION"));
    let mut app = App::new(config)?;

    let window_config = WindowConfig::new(app.config().app_name.clone(), 1280, 720);
    run_windowed(
        window_config,
        GameHandler {
            input: InputState::new(),
            gpu: None,
            scene: None,
        },
    )?;

    app.shutdown()?;
    Ok(())
}
