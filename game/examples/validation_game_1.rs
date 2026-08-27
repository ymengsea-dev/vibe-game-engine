//! Validation Game 1 — Stage 2's proof that the engine can make a genuinely
//! 2D game, not just draw a 3D demo.
//!
//! A simple collect/avoid/win-lose game: move the blue square with
//! WASD/arrow keys, touch the gold circles to collect them (touch a red
//! square instead and you lose), collect all of them to win. Exercises,
//! together for the first time, every Stage 2 piece: the orthographic
//! [`Camera`], the [`SpritePipeline`]/[`TextureAtlas`] sprite renderer, the
//! [`ActionMap`] action-mapping layer, [`Aabb2d`]/[`Circle2d`] 2D collision
//! checks, and the [`Scene`] format's [`SpriteData`] — the level layout is
//! built once, saved to a real `.ron` file on disk, then loaded back before
//! anything is spawned, so "saved and loaded as a real scene file" is
//! actually exercised, not just claimed.
//!
//! Run with `cargo run -p game --example validation_game_1`.
//!
//! Deliberately out of scope (see the feature's design notes): no on-screen
//! win/lose text (the engine has no text rendering yet — reported via
//! `tracing::info!` instead), no `AssetDatabase` integration (Stage 3/4
//! territory — the one atlas this game uses gets one [`AssetId`], not
//! looked up through a database), no wall-collision system (arena bounds
//! are a plain position clamp, not a collision check).

use std::sync::Arc;

use engine::ecs::components as ecs_components;
use engine::ecs::prelude::{Component, Entity, Resource, With, World};
use engine::prelude::*;
use glam::Vec3Swizzles;

/// Half the arena's width/height, in world units — the player's position
/// is clamped inside this box each frame (a plain clamp, not a collision
/// check: there's no wall geometry to collide with).
const ARENA_HALF_EXTENTS: glam::Vec2 = glam::Vec2::new(7.0, 4.0);
/// Orthographic camera visible height, in world units — taller than the
/// arena for a little margin.
const CAMERA_HEIGHT: f32 = 9.0;
/// Player movement speed, world units/second.
const MOVE_SPEED: f32 = 3.5;

const PLAYER_SIZE: glam::Vec2 = glam::Vec2::new(0.6, 0.6);
const HAZARD_SIZE: glam::Vec2 = glam::Vec2::new(0.6, 0.6);
const COLLECTIBLE_RADIUS: f32 = 0.3;

/// Atlas cell size, in pixels, and the grid region name
/// ([`AtlasLayout::add_grid`]'s `"{col}_{row}"` convention) each role
/// samples.
const CELL_SIZE: u32 = 32;
const PLAYER_REGION: &str = "0_0";
const COLLECTIBLE_REGION: &str = "1_0";
const HAZARD_REGION: &str = "2_0";

const PLAYER_COLOR: [u8; 4] = [80, 140, 255, 255];
const COLLECTIBLE_COLOR: [u8; 4] = [255, 210, 60, 255];
const HAZARD_COLOR: [u8; 4] = [230, 60, 60, 255];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

const COLLECTIBLE_POSITIONS: [glam::Vec2; 5] = [
    glam::Vec2::new(-5.0, 2.5),
    glam::Vec2::new(5.0, 2.5),
    glam::Vec2::new(-5.0, -2.5),
    glam::Vec2::new(5.0, -2.5),
    glam::Vec2::new(0.0, 3.2),
];
const HAZARD_POSITIONS: [glam::Vec2; 3] = [
    glam::Vec2::new(2.5, 0.0),
    glam::Vec2::new(-2.5, 0.0),
    glam::Vec2::new(0.0, -3.2),
];

/// The player-controlled entity. Marker only — game-local, not part of
/// `engine_ecs::components` (not a reusable engine concept).
#[derive(Component)]
struct Player;

/// A collectible entity — touching it with the player despawns it and
/// increments [`GameState::collected`].
#[derive(Component)]
struct Collectible;

/// A hazard entity — touching it with the player ends the game in
/// [`Phase::Lost`].
#[derive(Component)]
struct Hazard;

/// How the game is currently going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Playing,
    Won,
    Lost,
}

/// Score and win/lose state — an ECS resource so it's readable/writable
/// from the same `&mut World` everything else here already has.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
struct GameState {
    collected: u32,
    total_collectibles: u32,
    phase: Phase,
}

/// The actions this game's player can perform, bound to raw keys via
/// [`ActionMap`] — gameplay code below never names a [`KeyCode`] directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PlayerAction {
    Left,
    Right,
    Up,
    Down,
}

/// Builds a `colors.len() * cell_size` wide, `cell_size` tall RGBA8 image:
/// one solid-color square per entry in `colors`, left to right. Stand-in
/// for a real sprite-sheet asset — same role `checkerboard_rgba` plays in
/// `game/src/main.rs`'s 3D demo, procedural rather than a binary asset
/// file in the repo.
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

    let mut entities = vec![SceneEntity {
        name: Some("Player".to_string()),
        transform: Some(TransformData::from(Transform::from_translation(
            glam::Vec3::ZERO,
        ))),
        sprite: Some(SpriteData {
            atlas: atlas_ref.clone(),
            region: PLAYER_REGION.to_string(),
            size: PLAYER_SIZE.to_array(),
            color: WHITE,
        }),
        ..Default::default()
    }];

    for (index, position) in COLLECTIBLE_POSITIONS.iter().enumerate() {
        entities.push(SceneEntity {
            name: Some(format!("Collectible {index}")),
            transform: Some(TransformData::from(Transform::from_translation(
                position.extend(0.0),
            ))),
            sprite: Some(SpriteData {
                atlas: atlas_ref.clone(),
                region: COLLECTIBLE_REGION.to_string(),
                size: [COLLECTIBLE_RADIUS * 2.0, COLLECTIBLE_RADIUS * 2.0],
                color: WHITE,
            }),
            ..Default::default()
        });
    }

    for (index, position) in HAZARD_POSITIONS.iter().enumerate() {
        entities.push(SceneEntity {
            name: Some(format!("Hazard {index}")),
            transform: Some(TransformData::from(Transform::from_translation(
                position.extend(0.0),
            ))),
            sprite: Some(SpriteData {
                atlas: atlas_ref.clone(),
                region: HAZARD_REGION.to_string(),
                size: HAZARD_SIZE.to_array(),
                color: WHITE,
            }),
            ..Default::default()
        });
    }

    Scene {
        entities,
        ..Default::default()
    }
}

/// Spawns every sprite entity in `scene` into `world`, resolving each
/// [`SpriteData::region`] against `atlas_layout` and tagging [`Player`]/
/// [`Collectible`]/[`Hazard`] markers from the loaded [`SceneEntity::name`]
/// — classified once here at load time, not by re-matching strings every
/// frame.
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
            } else if name.starts_with("Collectible") {
                entity_mut.insert(Collectible);
            } else if name.starts_with("Hazard") {
                entity_mut.insert(Hazard);
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
            CELL_SIZE * 3,
            CELL_SIZE,
            &atlas_rgba(CELL_SIZE, &[PLAYER_COLOR, COLLECTIBLE_COLOR, HAZARD_COLOR]),
        );
        let mut atlas_layout = match AtlasLayout::new(CELL_SIZE * 3, CELL_SIZE) {
            Ok(layout) => layout,
            Err(err) => {
                tracing::error!(error = %err, "failed to create atlas layout");
                return;
            }
        };
        if let Err(err) = atlas_layout.add_grid(3, 1) {
            tracing::error!(error = %err, "failed to slice atlas into a grid");
            return;
        }
        let atlas = TextureAtlas::new(atlas_texture, atlas_layout);
        let atlas_binding = gpu.create_sprite_atlas_binding(&sprite_pipeline, &atlas);

        // Build the level, save it to a real file, then load it back —
        // "saved and loaded as a real scene file" exercised for real, not
        // just spawned directly from `build_level`'s in-memory `Scene`.
        let scene_path = std::env::temp_dir().join("vge-validation-game-1-scene.ron");
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
            collected: 0,
            total_collectibles: COLLECTIBLE_POSITIONS.len() as u32,
            phase: Phase::Playing,
        });

        let mut action_map: ActionMap<PlayerAction> = ActionMap::new();
        action_map
            .bind(PlayerAction::Left, Binding::Key(KeyCode::KeyA))
            .bind(PlayerAction::Left, Binding::Key(KeyCode::ArrowLeft))
            .bind(PlayerAction::Right, Binding::Key(KeyCode::KeyD))
            .bind(PlayerAction::Right, Binding::Key(KeyCode::ArrowRight))
            .bind(PlayerAction::Up, Binding::Key(KeyCode::KeyW))
            .bind(PlayerAction::Up, Binding::Key(KeyCode::ArrowUp))
            .bind(PlayerAction::Down, Binding::Key(KeyCode::KeyS))
            .bind(PlayerAction::Down, Binding::Key(KeyCode::ArrowDown));

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
                    let mut horizontal = glam::Vec2::ZERO;
                    if scene.action_map.is_held(PlayerAction::Left, &self.input) {
                        horizontal.x -= 1.0;
                    }
                    if scene.action_map.is_held(PlayerAction::Right, &self.input) {
                        horizontal.x += 1.0;
                    }
                    if scene.action_map.is_held(PlayerAction::Up, &self.input) {
                        horizontal.y += 1.0;
                    }
                    if scene.action_map.is_held(PlayerAction::Down, &self.input) {
                        horizontal.y -= 1.0;
                    }
                    let delta = horizontal.normalize_or_zero() * MOVE_SPEED * dt;

                    let mut player_pos = glam::Vec2::ZERO;
                    {
                        let world = scene.ecs.world_mut();
                        let mut player_query =
                            world.query_filtered::<&mut ecs_components::Transform, With<Player>>();
                        if let Ok(mut transform) = player_query.single_mut(world) {
                            let pos = (transform.0.translation.xy() + delta)
                                .clamp(-ARENA_HALF_EXTENTS, ARENA_HALF_EXTENTS);
                            transform.0.translation = pos.extend(0.0);
                            player_pos = pos;
                        }
                    }

                    let player_aabb = Aabb2d::new(player_pos, PLAYER_SIZE);

                    let mut collected_entities: Vec<Entity> = Vec::new();
                    {
                        let world = scene.ecs.world_mut();
                        let mut collectible_query = world
                            .query_filtered::<(Entity, &ecs_components::Transform), With<Collectible>>(
                            );
                        for (entity, transform) in collectible_query.iter(world) {
                            let circle =
                                Circle2d::new(transform.0.translation.xy(), COLLECTIBLE_RADIUS);
                            if aabb_vs_circle(&player_aabb, &circle) {
                                collected_entities.push(entity);
                            }
                        }
                    }
                    let collected_now = collected_entities.len() as u32;
                    for entity in collected_entities {
                        scene.ecs.world_mut().despawn(entity);
                    }

                    let mut hit_hazard = false;
                    {
                        let world = scene.ecs.world_mut();
                        let mut hazard_query =
                            world.query_filtered::<&ecs_components::Transform, With<Hazard>>();
                        for transform in hazard_query.iter(world) {
                            let hazard_aabb =
                                Aabb2d::new(transform.0.translation.xy(), HAZARD_SIZE);
                            if aabb_vs_aabb(&player_aabb, &hazard_aabb) {
                                hit_hazard = true;
                                break;
                            }
                        }
                    }

                    if let Some(state) = scene.ecs.resource_mut::<GameState>() {
                        state.collected += collected_now;
                        if hit_hazard {
                            state.phase = Phase::Lost;
                            tracing::info!("you lose — touched a hazard");
                        } else if state.collected >= state.total_collectibles {
                            state.phase = Phase::Won;
                            tracing::info!(collected = state.collected, "you win!");
                        }
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

    let config = EngineConfig::new("VGE Validation Game 1", env!("CARGO_PKG_VERSION"));
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
