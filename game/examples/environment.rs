//! Validation Game 5 — a procedural environment demo on `engine::app`.
//!
//! `cargo run -p game --example environment`
//!
//! A walkable terrain patch: `Heightmap`-driven relief, `scatter`-placed
//! grass (wind-animated) and rocks, and a couple of ember particle
//! fountains. No gameplay goal.
//!
//! - **WASD** walks a ground cursor over the terrain; the [`CameraRig`]
//!   follows it, **arrow keys** orbit.
//! - **`[` / `]`** lower / raise the terrain under the cursor (live
//!   sculpt + re-upload).
//! - **Esc** quits.
//!
//! Validates: terrain + sculpt, vegetation rendering, procedural
//! scattering, particles — all through `run_game`'s frame loop.

use engine::prelude::*;

/// World-space Y the terrain entity sits at (its heightmap origin is
/// world `(0, 0)` on XZ).
const TERRAIN_Y: f32 = -1.5;
/// Local edge length of the square heightmap patch.
const TERRAIN_SIZE: f32 = 40.0;
/// Vertices per side.
const TERRAIN_RESOLUTION: u32 = 96;
/// How far the cursor can roam from the origin on X and Z.
const ROAM_HALF: f32 = 16.0;

fn main() -> Result<(), GameError> {
    let _ = logging::init_default();
    run_game(
        GameConfig::new("VGE — Procedural Environment", 1280, 720),
        EnvironmentDemo::default(),
    )
}

#[derive(Default)]
struct EnvironmentDemo {
    heightmap: Option<Heightmap>,
    terrain_mesh: Option<AssetHandle<Mesh>>,
    cursor: glam::Vec2,
    rig: Option<CameraRig>,
}

impl Game for EnvironmentDemo {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        let white = ctx
            .gpu()
            .create_texture_from_rgba("white", 1, 1, &[255, 255, 255, 255]);

        // --- terrain ---
        let heightmap = Heightmap::from_fn(TERRAIN_RESOLUTION, TERRAIN_SIZE, |row, col| {
            let u = col as f32 / (TERRAIN_RESOLUTION - 1) as f32;
            let v = row as f32 / (TERRAIN_RESOLUTION - 1) as f32;
            let tau = std::f32::consts::TAU;
            (u * tau * 1.5).sin() * 0.8 + (v * tau * 1.1).cos() * 0.6
        })
        .map_err(|err| GameError::Setup(err.to_string()))?;

        let (terrain_vertices, terrain_indices) = heightmap.mesh_data();
        let terrain_mesh = ctx
            .gpu()
            .create_mesh_dynamic("terrain", &terrain_vertices, &terrain_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        let terrain_entity = ctx.spawn_mesh(
            terrain_mesh,
            &white,
            material([0.42, 0.55, 0.36, 1.0], 0.95),
            Transform::from_translation(glam::Vec3::new(0.0, TERRAIN_Y, 0.0)),
        );
        self.terrain_mesh = ctx
            .world()
            .get::<engine::ecs::components::MeshRenderer>(terrain_entity)
            .map(|renderer| renderer.mesh);

        // --- scattered grass (wind-animated vegetation) ---
        let mut grass_config = ScatterConfig::new(
            ScatterArea::new(
                glam::Vec2::splat(-ROAM_HALF),
                glam::Vec2::splat(ROAM_HALF * 2.0),
            ),
            0.7,
        );
        grass_config.scale_range = (0.7, 1.5);
        grass_config.slope_limit = Some(33.0_f32.to_radians());
        grass_config.terrain_y_offset = TERRAIN_Y;
        let blades = scatter(&grass_config, 0x5F3E_11A2, Some(&heightmap))
            .map_err(|err| GameError::Setup(err.to_string()))?;
        tracing::info!(count = blades.len(), "scattered grass");
        let (grass_vertices, grass_indices) = grass_blade();
        let grass_mesh = ctx
            .gpu()
            .create_mesh("grass blade", &grass_vertices, &grass_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        ctx.spawn_vegetation(
            grass_mesh,
            &white,
            material([0.34, 0.68, 0.28, 1.0], 1.0),
            blades,
        );

        // --- scattered rocks (tilted to the surface) ---
        let mut rock_config = ScatterConfig::new(
            ScatterArea::new(
                glam::Vec2::splat(-ROAM_HALF),
                glam::Vec2::splat(ROAM_HALF * 2.0),
            ),
            3.0,
        );
        rock_config.scale_range = (0.25, 0.6);
        rock_config.density = 0.6;
        rock_config.align_to_normal = 0.7;
        rock_config.slope_limit = Some(62.0_f32.to_radians());
        rock_config.terrain_y_offset = TERRAIN_Y + 0.15;
        let rocks = scatter(&rock_config, 0x2B7C_9D04, Some(&heightmap))
            .map_err(|err| GameError::Setup(err.to_string()))?;
        tracing::info!(count = rocks.len(), "scattered rocks");
        let (cube_vertices, cube_indices) = cube();
        let rock_mesh = ctx
            .gpu()
            .create_mesh("rock", &cube_vertices, &cube_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        ctx.spawn_mesh_instanced(
            rock_mesh,
            &white,
            material([0.5, 0.5, 0.52, 1.0], 0.9),
            rocks,
        );

        // --- ember fountains ---
        for spot in [[-6.0, -4.0], [7.0, 5.0], [1.0, 8.0]] {
            let y = TERRAIN_Y + heightmap.height_at(spot[0], spot[1]);
            ctx.spawn((
                engine::ecs::components::Transform::from(Transform::from_translation(
                    glam::Vec3::new(spot[0], y, spot[1]),
                )),
                ParticleEmitter::new(ParticleEmitterConfig {
                    direction: glam::Vec3::new(0.1, 1.0, 0.05),
                    spawn_rate: 45.0,
                    ..ParticleEmitterConfig::EMBERS
                }),
            ));
        }

        // --- camera + a warm fill light ---
        let mut rig = CameraRig::new(glam::Vec3::ZERO);
        rig.distance = 10.0;
        rig.height = 2.5;
        self.rig = Some(rig);

        ctx.lights_mut().point.push(PointLight {
            position: glam::Vec3::new(4.0, 8.0, 4.0),
            color: glam::Vec3::new(1.0, 0.9, 0.7),
            intensity: 22.0,
            range: 50.0,
        });

        self.heightmap = Some(heightmap);
        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if ctx.input().is_key_held(KeyCode::Escape) {
            ctx.request_exit();
            return;
        }
        let (Some(heightmap), Some(rig)) = (self.heightmap.as_mut(), self.rig.as_mut()) else {
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

        // --- walk the cursor over the terrain ---
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
        self.cursor += glam::Vec2::new(world_move.x, world_move.z) * 6.0 * dt;
        self.cursor.x = self.cursor.x.clamp(-ROAM_HALF, ROAM_HALF);
        self.cursor.y = self.cursor.y.clamp(-ROAM_HALF, ROAM_HALF);

        // --- live terrain sculpt ---
        let mut sculpted = false;
        if ctx.input().is_key_held(KeyCode::BracketRight) {
            heightmap.raise_lower(&Brush::new(self.cursor, 3.0, 3.0 * dt));
            sculpted = true;
        }
        if ctx.input().is_key_held(KeyCode::BracketLeft) {
            heightmap.raise_lower(&Brush::new(self.cursor, 3.0, -3.0 * dt));
            sculpted = true;
        }
        if sculpted && let Some(handle) = self.terrain_mesh {
            let vertices = heightmap.vertices();
            ctx.rewrite_mesh(handle, &vertices);
        }

        // --- camera follows the cursor's ground point ---
        let ground_y = TERRAIN_Y + heightmap.height_at(self.cursor.x, self.cursor.y);
        rig.target = glam::Vec3::new(self.cursor.x, ground_y, self.cursor.y);
        rig.apply(ctx.camera_mut(), dt);
    }
}

fn material(base_color_factor: [f32; 4], roughness_factor: f32) -> Material {
    Material {
        base_color_factor,
        metallic_factor: 0.0,
        roughness_factor,
    }
}

/// One grass blade: two crossed quads, base at `y = 0`, ~0.5 tall — the
/// layout the vegetation wind shader assumes.
fn grass_blade() -> (Vec<Vertex>, Vec<u32>) {
    let half_width = 0.06_f32;
    let height = 0.5_f32;
    let mut vertices = Vec::with_capacity(8);
    let mut indices = Vec::with_capacity(12);
    for (axis, normal) in [
        (glam::Vec3::X, [0.0, 0.0, 1.0]),
        (glam::Vec3::Z, [1.0, 0.0, 0.0]),
    ] {
        let edge = axis * half_width;
        let base = vertices.len() as u32;
        for (offset, y, uv) in [
            (-edge, 0.0, [0.0, 1.0]),
            (edge, 0.0, [1.0, 1.0]),
            (edge, height, [1.0, 0.0]),
            (-edge, height, [0.0, 0.0]),
        ] {
            vertices.push(Vertex {
                position: [offset.x, y, offset.z],
                normal,
                uv,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}
