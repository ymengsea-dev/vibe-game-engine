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
//! Rebuilt on [`run_game`] (T-08): the window, GPU context, sprite
//! pipeline, camera binding and frame loop are all the engine's now, so
//! what is left below is level data plus gameplay. A 2D game is a game
//! with an orthographic camera and no meshes — not a separate code path.
//!
//! Deliberately out of scope (see the feature's design notes): no on-screen
//! win/lose text (the engine has no text rendering yet — reported via
//! `tracing::info!` instead), no `AssetDatabase` integration (Stage 3/4
//! territory — the one atlas this game uses gets one [`AssetId`], not
//! looked up through a database), no wall-collision system (arena bounds
//! are a plain position clamp, not a collision check).

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

/// Sprite layers ([`Sprite::z_order`]): the player draws over the pickups,
/// which draw over the hazards. Explicit, so layering does not depend on
/// the order entities happen to be spawned in.
const HAZARD_LAYER: f32 = 0.0;
const COLLECTIBLE_LAYER: f32 = 1.0;
const PLAYER_LAYER: f32 = 2.0;

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
            z_order: PLAYER_LAYER,
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
                z_order: COLLECTIBLE_LAYER,
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
                z_order: HAZARD_LAYER,
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
        sprite.z_order = sprite_data.z_order;

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

/// The game: an action map plus the atlas layout the level was spawned
/// from. Everything else lives in the ECS world the engine owns.
struct ValidationGame {
    action_map: ActionMap<PlayerAction>,
}

impl Game for ValidationGame {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        // One procedural atlas: three solid cells, sliced into a grid.
        let atlas_texture = ctx.gpu().create_texture_from_rgba(
            "sprite atlas",
            CELL_SIZE * 3,
            CELL_SIZE,
            &atlas_rgba(CELL_SIZE, &[PLAYER_COLOR, COLLECTIBLE_COLOR, HAZARD_COLOR]),
        );
        let mut atlas_layout = AtlasLayout::new(CELL_SIZE * 3, CELL_SIZE)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        atlas_layout
            .add_grid(3, 1)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        let atlas = TextureAtlas::new(atlas_texture, atlas_layout);
        ctx.set_sprite_atlas(&atlas);

        // Build the level, save it to a real file, then load it back —
        // "saved and loaded as a real scene file" exercised for real, not
        // just spawned directly from `build_level`'s in-memory `Scene`.
        let scene_path = std::env::temp_dir().join("vge-validation-game-1-scene.ron");
        let level = build_level(AssetId::new());
        level
            .save_to_file(&scene_path)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        tracing::info!(path = %scene_path.display(), "saved validation game scene");

        let loaded_scene =
            Scene::load_from_file(&scene_path).map_err(|err| GameError::Setup(err.to_string()))?;
        tracing::info!(
            path = %scene_path.display(),
            entities = loaded_scene.entities.len(),
            "loaded validation game scene"
        );

        let layout = atlas.layout.clone();
        let world = ctx.world_mut();
        spawn_from_scene(world, &loaded_scene, &layout);
        world.insert_resource(GameState {
            collected: 0,
            total_collectibles: COLLECTIBLE_POSITIONS.len() as u32,
            phase: Phase::Playing,
        });
        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        let phase = ctx
            .world()
            .get_resource::<GameState>()
            .map_or(Phase::Lost, |state| state.phase);
        if phase != Phase::Playing {
            return;
        }

        let mut horizontal = glam::Vec2::ZERO;
        if self.action_map.is_held(PlayerAction::Left, ctx.input()) {
            horizontal.x -= 1.0;
        }
        if self.action_map.is_held(PlayerAction::Right, ctx.input()) {
            horizontal.x += 1.0;
        }
        if self.action_map.is_held(PlayerAction::Up, ctx.input()) {
            horizontal.y += 1.0;
        }
        if self.action_map.is_held(PlayerAction::Down, ctx.input()) {
            horizontal.y -= 1.0;
        }
        let delta = horizontal.normalize_or_zero() * MOVE_SPEED * dt;

        let world = ctx.world_mut();
        let mut player_pos = glam::Vec2::ZERO;
        {
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
            let mut collectible_query =
                world.query_filtered::<(Entity, &ecs_components::Transform), With<Collectible>>();
            for (entity, transform) in collectible_query.iter(world) {
                let circle = Circle2d::new(transform.0.translation.xy(), COLLECTIBLE_RADIUS);
                if aabb_vs_circle(&player_aabb, &circle) {
                    collected_entities.push(entity);
                }
            }
        }
        let collected_now = collected_entities.len() as u32;
        for entity in collected_entities {
            world.despawn(entity);
        }

        let mut hit_hazard = false;
        {
            let mut hazard_query =
                world.query_filtered::<&ecs_components::Transform, With<Hazard>>();
            for transform in hazard_query.iter(world) {
                let hazard_aabb = Aabb2d::new(transform.0.translation.xy(), HAZARD_SIZE);
                if aabb_vs_aabb(&player_aabb, &hazard_aabb) {
                    hit_hazard = true;
                    break;
                }
            }
        }

        if let Some(mut state) = world.get_resource_mut::<GameState>() {
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init_default()?;

    let mut config = GameConfig::new("VGE Validation Game 1", 1280, 720);
    // The one thing that makes this a 2D game: an orthographic camera
    // looking down -Z at the XY plane. No meshes are ever spawned, so
    // the 3D half of the frame costs nothing.
    config.camera = Camera::new_orthographic(
        glam::Vec3::new(0.0, 0.0, 5.0),
        glam::Vec3::ZERO,
        config.width as f32 / config.height as f32,
        CAMERA_HEIGHT,
    );

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

    run_game(config, ValidationGame { action_map })?;
    Ok(())
}
