//! Validation Game 4 — a small third-person game on `engine::app`.
//!
//! `cargo run -p game --example third_person`
//!
//! - **WASD** moves the character relative to the camera; hold **Shift**
//!   to run. The character turns to face its motion and plays an
//!   idle/walk/run animation driven by an `engine_animation::StateMachine`.
//! - **Arrow keys** orbit the third-person [`CameraRig`] follow camera.
//! - Walk onto a glowing cube and press **E** to collect it; grab all
//!   five to win.
//! - A cube NPC pathfinds toward the character around the obstacles
//!   (`engine_ai` navigation).
//! - **Esc** quits.
//!
//! Validates: the gameplay API, third-person camera, animation
//! state machine + GPU skinning under gameplay, ECS spawn/despawn/query,
//! and navigation with a moving goal.

use std::collections::HashMap;

use engine::prelude::*;

/// Half-width of the square play area on X and Z.
const ARENA_HALF: f32 = 14.0;
/// How close the character must be to a collectible to grab it.
const PICKUP_RANGE: f32 = 1.6;

fn main() -> Result<(), GameError> {
    let _ = logging::init_default();
    run_game(
        GameConfig::new("VGE — Third Person", 1280, 720),
        ThirdPerson::default(),
    )
}

#[derive(Default)]
struct ThirdPerson {
    // Leaked to `'static` so the borrowing `StateMachine` can live in this
    // struct without a self-reference. One small leak, at setup, for a
    // run-once example.
    skeleton: Option<&'static ImportedSkeleton>,
    machine: Option<StateMachine<'static>>,

    character: Option<Entity>,
    char_pos: glam::Vec3,
    char_yaw: f32,

    rig: Option<CameraRig>,

    collectibles: Vec<(Entity, glam::Vec2)>,
    collected: usize,
    won: bool,

    nav_grid: Option<NavGrid>,
    npc: Option<Entity>,
    npc_pos: glam::Vec2,
    npc_path: Vec<glam::Vec2>,
    npc_replan_accumulator: f32,
}

impl Game for ThirdPerson {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        let white = ctx
            .gpu()
            .create_texture_from_rgba("white", 1, 1, &[255, 255, 255, 255]);
        let (cube_vertices, cube_indices) = cube();

        // Ground.
        let ground = ctx
            .gpu()
            .create_mesh("ground", &cube_vertices, &cube_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        ctx.spawn_mesh(
            ground,
            &white,
            material([0.36, 0.5, 0.34, 1.0], 0.9),
            Transform::from_translation(glam::Vec3::new(0.0, -0.5, 0.0))
                .with_scale(glam::Vec3::new(ARENA_HALF * 2.2, 1.0, ARENA_HALF * 2.2)),
        );

        // Obstacles + a nav grid with their footprints blocked.
        let mut nav_grid = NavGrid::new(40, 40, 0.75, glam::Vec2::splat(-15.0))
            .map_err(|err| GameError::Setup(err.to_string()))?;
        for centre in OBSTACLES {
            nav_grid.block_circle(glam::Vec2::new(centre[0], centre[1]), 1.4);
            let obstacle = ctx
                .gpu()
                .create_mesh("obstacle", &cube_vertices, &cube_indices)
                .map_err(|err| GameError::Setup(err.to_string()))?;
            ctx.spawn_mesh(
                obstacle,
                &white,
                material([0.5, 0.42, 0.38, 1.0], 0.85),
                Transform::from_translation(glam::Vec3::new(centre[0], 0.6, centre[1]))
                    .with_scale(glam::Vec3::new(1.6, 1.3, 1.6)),
            );
        }
        self.nav_grid = Some(nav_grid);

        // The skinned character: a two-joint bar. Skeleton and the three
        // locomotion clips are leaked so the `StateMachine` can borrow
        // them for the run.
        let (skinned_vertices, skinned_indices, skeleton) = skinned_bar();
        let skinned_mesh = ctx
            .gpu()
            .create_skinned_mesh("character", &skinned_vertices, &skinned_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        self.character = Some(ctx.spawn_skinned_mesh(
            skinned_mesh,
            &white,
            material([0.85, 0.6, 0.3, 1.0], 0.5),
            Transform::from_scale(glam::Vec3::splat(0.5)),
        ));

        let skeleton: &'static ImportedSkeleton = Box::leak(Box::new(skeleton));
        let idle: &'static ImportedAnimation = Box::leak(Box::new(bend_clip("idle", 3.0, 0.08)));
        let walk: &'static ImportedAnimation = Box::leak(Box::new(bend_clip("walk", 1.1, 0.5)));
        let run: &'static ImportedAnimation = Box::leak(Box::new(bend_clip("run", 0.6, 0.95)));
        self.skeleton = Some(skeleton);
        self.machine = Some(
            StateMachine::new(
                vec![
                    AnimationState {
                        name: "idle".to_string(),
                        clip: idle,
                        transitions: vec![Transition {
                            target: "walk".to_string(),
                            conditions: vec![condition("moving", true)],
                            duration_seconds: 0.15,
                        }],
                    },
                    AnimationState {
                        name: "walk".to_string(),
                        clip: walk,
                        transitions: vec![
                            Transition {
                                target: "run".to_string(),
                                conditions: vec![condition("running", true)],
                                duration_seconds: 0.15,
                            },
                            Transition {
                                target: "idle".to_string(),
                                conditions: vec![condition("moving", false)],
                                duration_seconds: 0.2,
                            },
                        ],
                    },
                    AnimationState {
                        name: "run".to_string(),
                        clip: run,
                        transitions: vec![Transition {
                            target: "walk".to_string(),
                            conditions: vec![condition("running", false)],
                            duration_seconds: 0.2,
                        }],
                    },
                ],
                "idle",
            )
            .map_err(|err| GameError::Setup(err.to_string()))?,
        );

        // Collectibles.
        for spot in COLLECTIBLES {
            let mesh = ctx
                .gpu()
                .create_mesh("collectible", &cube_vertices, &cube_indices)
                .map_err(|err| GameError::Setup(err.to_string()))?;
            let entity = ctx.spawn_mesh(
                mesh,
                &white,
                material([1.0, 0.85, 0.3, 1.0], 0.3),
                Transform::from_translation(glam::Vec3::new(spot[0], 0.6, spot[1]))
                    .with_scale(glam::Vec3::splat(0.35)),
            );
            self.collectibles
                .push((entity, glam::Vec2::new(spot[0], spot[1])));
        }

        // NPC.
        let npc_mesh = ctx
            .gpu()
            .create_mesh("npc", &cube_vertices, &cube_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        self.npc_pos = glam::Vec2::new(-11.0, -11.0);
        self.npc = Some(
            ctx.spawn_mesh(
                npc_mesh,
                &white,
                material([0.3, 0.55, 0.95, 1.0], 0.4),
                Transform::from_translation(glam::Vec3::new(self.npc_pos.x, 0.4, self.npc_pos.y))
                    .with_scale(glam::Vec3::splat(0.45)),
            ),
        );

        let mut rig = CameraRig::new(self.char_pos);
        rig.distance = 8.0;
        rig.height = 2.0;
        self.rig = Some(rig);

        ctx.lights_mut().point.push(PointLight {
            position: glam::Vec3::new(3.0, 6.0, 3.0),
            color: glam::Vec3::new(1.0, 0.9, 0.75),
            intensity: 18.0,
            range: 40.0,
        });

        Ok(())
    }

    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, step: f32) {
        let (Some(nav_grid), Some(npc)) = (self.nav_grid.as_ref(), self.npc) else {
            return;
        };

        self.npc_replan_accumulator += step;
        if self.npc_replan_accumulator >= 0.5 {
            self.npc_replan_accumulator = 0.0;
            let start = nav_grid.world_to_coord(self.npc_pos);
            let goal = nav_grid.world_to_coord(glam::Vec2::new(self.char_pos.x, self.char_pos.z));
            match find_path(nav_grid, start, goal) {
                Some(coords) => {
                    self.npc_path = smooth_path(nav_grid, &coords)
                        .iter()
                        .map(|coord| nav_grid.coord_to_world_center(*coord))
                        .collect();
                }
                None => self.npc_path.clear(),
            }
        }

        let (new_pos, reached) = follow_path(self.npc_pos, &self.npc_path, 2.2, step);
        self.npc_pos = new_pos;
        if reached > 0 {
            self.npc_path.drain(..reached.min(self.npc_path.len()));
        }
        if let Some(mut transform) = ctx
            .world_mut()
            .get_mut::<engine::ecs::components::Transform>(npc)
        {
            transform.0.translation = glam::Vec3::new(self.npc_pos.x, 0.4, self.npc_pos.y);
        }
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if ctx.input().is_key_held(KeyCode::Escape) {
            ctx.request_exit();
            return;
        }
        let (Some(character), Some(rig), Some(machine), Some(skeleton)) = (
            self.character,
            self.rig.as_mut(),
            self.machine.as_mut(),
            self.skeleton,
        ) else {
            return;
        };

        // --- camera orbit ---
        let mut orbit_yaw = 0.0;
        let mut orbit_pitch = 0.0;
        if ctx.input().is_key_held(KeyCode::ArrowLeft) {
            orbit_yaw += 1.6 * dt;
        }
        if ctx.input().is_key_held(KeyCode::ArrowRight) {
            orbit_yaw -= 1.6 * dt;
        }
        if ctx.input().is_key_held(KeyCode::ArrowUp) {
            orbit_pitch += 1.0 * dt;
        }
        if ctx.input().is_key_held(KeyCode::ArrowDown) {
            orbit_pitch -= 1.0 * dt;
        }
        rig.orbit(orbit_yaw, orbit_pitch);

        // --- movement, camera-relative ---
        let mut local = glam::Vec3::ZERO;
        if ctx.input().is_key_held(KeyCode::KeyW) {
            local.z -= 1.0;
        }
        if ctx.input().is_key_held(KeyCode::KeyS) {
            local.z += 1.0;
        }
        if ctx.input().is_key_held(KeyCode::KeyA) {
            local.x -= 1.0;
        }
        if ctx.input().is_key_held(KeyCode::KeyD) {
            local.x += 1.0;
        }
        let world_move = (glam::Quat::from_rotation_y(rig.yaw) * local).normalize_or_zero();
        let moving = world_move.length_squared() > 0.01;
        let running = moving && ctx.input().is_key_held(KeyCode::ShiftLeft);
        let speed = if running { 6.5 } else { 3.5 };

        if moving {
            self.char_pos += world_move * speed * dt;
            self.char_pos.x = self.char_pos.x.clamp(-ARENA_HALF, ARENA_HALF);
            self.char_pos.z = self.char_pos.z.clamp(-ARENA_HALF, ARENA_HALF);
            let target_yaw = world_move.x.atan2(world_move.z);
            self.char_yaw = approach_angle(self.char_yaw, target_yaw, 10.0 * dt);
        }

        if let Some(mut transform) = ctx
            .world_mut()
            .get_mut::<engine::ecs::components::Transform>(character)
        {
            transform.0.translation = self.char_pos;
            transform.0.rotation = glam::Quat::from_rotation_y(self.char_yaw);
        }

        // --- animation ---
        machine.set_parameter("moving", moving);
        machine.set_parameter("running", running);
        machine.update(dt);
        let pose = machine.sample(skeleton);
        let matrices = compute_skinning_matrices(skeleton, &pose);
        if let Some(renderer) = ctx
            .world()
            .get::<engine::ecs::components::SkinnedMeshRenderer>(character)
        {
            ctx.gpu().write_uniform_buffer(
                &renderer.skin.joints_buffer,
                &JointMatricesUniform::from_matrices(&matrices),
            );
        }

        // --- camera follow ---
        rig.target = self.char_pos;
        rig.apply(ctx.camera_mut(), dt);

        // --- collectibles: spin, then pick up ---
        for &(entity, _) in &self.collectibles {
            if let Some(mut transform) = ctx
                .world_mut()
                .get_mut::<engine::ecs::components::Transform>(entity)
            {
                transform.0.rotation *= glam::Quat::from_rotation_y(dt * 3.0);
            }
        }
        if ctx.input().is_key_held(KeyCode::KeyE) {
            let character_xz = glam::Vec2::new(self.char_pos.x, self.char_pos.z);
            let mut i = 0;
            while i < self.collectibles.len() {
                let (entity, position) = self.collectibles[i];
                if character_xz.distance(position) < PICKUP_RANGE {
                    ctx.world_mut().despawn(entity);
                    self.collectibles.swap_remove(i);
                    self.collected += 1;
                } else {
                    i += 1;
                }
            }
        }
        if !self.won && self.collectibles.is_empty() {
            self.won = true;
            tracing::info!(
                collected = self.collected,
                "all collectibles picked up — you win"
            );
        }
    }
}

/// Fixed obstacle centres, shared by the meshes and the nav grid.
const OBSTACLES: [[f32; 2]; 4] = [[-4.0, -2.0], [3.0, 4.0], [5.0, -5.0], [-2.0, 6.0]];

/// Where the five collectibles sit.
const COLLECTIBLES: [[f32; 2]; 5] = [
    [7.0, 7.0],
    [-8.0, 3.0],
    [2.0, -8.0],
    [-6.0, -7.0],
    [9.0, -2.0],
];

fn material(base_color_factor: [f32; 4], roughness_factor: f32) -> Material {
    Material {
        base_color_factor,
        metallic_factor: 0.0,
        roughness_factor,
        ..Material::DEFAULT
    }
}

fn condition(parameter: &str, required: bool) -> TransitionCondition {
    TransitionCondition {
        parameter: parameter.to_string(),
        required,
    }
}

/// The shortest signed step from `current` toward `target` (wrapping at
/// +/-PI), clamped to `max_step`.
fn approach_angle(current: f32, target: f32, max_step: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let pi = std::f32::consts::PI;
    let diff = (target - current + pi).rem_euclid(tau) - pi;
    current + diff.clamp(-max_step, max_step)
}

/// A two-joint skinned bar (root + bending tip), plus its skeleton —
/// adapted from `game/src/main.rs`'s `skinned_bar`.
fn skinned_bar() -> (Vec<SkinnedVertex>, Vec<u32>, ImportedSkeleton) {
    let (vertices, indices) = cube();
    let skinned_vertices = vertices
        .into_iter()
        .map(|vertex| {
            let y = (vertex.position[1] + 0.5) * 2.0;
            let (joints, weights) = if y < 1.0 {
                ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
            } else {
                ([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
            };
            SkinnedVertex {
                position: [vertex.position[0] * 0.5, y, vertex.position[2] * 0.5],
                normal: vertex.normal,
                uv: vertex.uv,
                joints,
                weights,
            }
        })
        .collect();

    let skeleton = ImportedSkeleton {
        name: Some("bar".to_string()),
        joints: vec![
            ImportedJoint {
                name: Some("root".to_string()),
                node_index: 0,
                parent: None,
                local_bind_transform: Transform::IDENTITY,
                inverse_bind_matrix: glam::Mat4::IDENTITY,
            },
            ImportedJoint {
                name: Some("tip".to_string()),
                node_index: 1,
                parent: Some(0),
                local_bind_transform: Transform::from_translation(glam::Vec3::new(0.0, 1.0, 0.0)),
                inverse_bind_matrix: glam::Mat4::from_translation(glam::Vec3::new(0.0, -1.0, 0.0)),
            },
        ],
    };

    (skinned_vertices, indices, skeleton)
}

/// A looping clip that swings joint 1 about `Z` from straight to
/// `tip_angle` and back over `duration` seconds.
fn bend_clip(name: &str, duration: f32, tip_angle: f32) -> ImportedAnimation {
    let mut channels = HashMap::new();
    channels.insert(
        1,
        ImportedAnimationChannels {
            translation: None,
            rotation: Some(ImportedKeyframes {
                interpolation: ImportedInterpolation::Linear,
                times: vec![0.0, duration * 0.5, duration],
                values: vec![
                    glam::Quat::IDENTITY,
                    glam::Quat::from_rotation_z(tip_angle),
                    glam::Quat::IDENTITY,
                ],
            }),
            scale: None,
        },
    );
    ImportedAnimation {
        name: Some(name.to_string()),
        duration,
        channels,
    }
}
