//! Example game project for the Vibe Game Engine (VGE).
//!
//! This binary is the engine's first consumer: each milestone extends it
//! to exercise newly implemented engine features.

use std::collections::HashMap;
use std::sync::Arc;

use engine::ecs::components as ecs_components;
use engine::ecs::prelude::{ChildOf, Entity};
use engine::prelude::*;

/// World-space Y the navigation-demo walker and obstacle cubes sit at —
/// a flat plane above the rolling terrain, since the demo is about
/// pathfinding, not physics.
const WALKER_Y: f32 = -0.55;

/// GPU resources and the ECS world for drawing the example scene.
///
/// The renderer no longer holds a direct reference to "the cube" — it
/// draws whatever `(Transform, MeshRenderer)` entities exist in `ecs`
/// (see `engine_ecs::extract_and_render`). `pipeline`/`camera`/
/// `camera_binding` stay outside the ECS: one shared pipeline and one
/// camera for the whole frame, not per-entity data.
struct Scene {
    /// GPU-resident meshes/material bindings that every `MeshRenderer`
    /// component in `ecs` resolves its handles against — see
    /// `engine_ecs::components::MeshRenderer`'s docs. Owned here (not
    /// inside `ecs`) for the same reason `pipeline`/`camera` are: exactly
    /// one per renderer, not per-`World` state.
    render_assets: RenderAssets,
    pipeline: Pipeline,
    shadow_pipeline: ShadowPipeline,
    shadow_map: ShadowMap,
    skybox_pipeline: SkyboxPipeline,
    skybox: SkyboxBinding,
    hdr_target: HdrTarget,
    /// The Stage 5 post-processing stack — bloom, color grade, toon
    /// outline, then ACES tonemap — that replaced the standalone tonemap
    /// pass. Rebuilt on resize alongside `hdr_target` (both are
    /// window-sized). See `engine_renderer::PostProcessStack`.
    post: PostProcessStack,
    /// The shadow-casting/skybox-sun light's direction and color, kept
    /// around so [`GameHandler::on_event`]'s resize handler can recompute
    /// the skybox uniform (its inverse view-projection depends on aspect
    /// ratio) without needing the whole [`LightSet`] around.
    sun_direction: glam::Vec3,
    sun_color: glam::Vec3,
    camera: Camera,
    camera_binding: CameraBinding,
    lights_binding: LightsBinding,
    ecs: Ecs,
    physics: PhysicsWorld,
    /// Wall-clock time of the last [`PlatformEvent::RedrawRequested`], for
    /// measuring the real time each frame took.
    last_frame: std::time::Instant,
    /// Fixed-step accumulator: the real frame time feeds this, and physics
    /// (plus the character/gravity integration below) runs a whole number
    /// of equal-sized steps per frame so simulation behavior doesn't
    /// change with the frame rate.
    fixed_timestep: FixedTimestep,
    /// The GPU-skinned pipeline, and the one animated bar drawn through it.
    skinned_pipeline: SkinnedPipeline,
    /// The instancing pipeline — draws the 64-cube grid in one call.
    instanced_pipeline: InstancedPipeline,
    /// The alpha + additive billboard-particle pipelines, and the
    /// billboard-basis camera binding rewritten each frame — draws the
    /// ember emitter spawned into `ecs`.
    particle_pipelines: ParticlePipelines,
    particle_camera: ParticleCameraBinding,
    /// The editable heightmap behind the terrain mesh, and the handle of
    /// the dynamic `Mesh` it uploads into. `[` / `]` / `\` sculpt it at
    /// the player's position each frame; the whole mesh is re-uploaded via
    /// `write_mesh_vertices` after any edit.
    terrain_heightmap: Heightmap,
    terrain_mesh: AssetHandle<Mesh>,
    /// The wind-animated vegetation pipeline, the global wind params, its
    /// GPU binding, and a bounded time accumulator driving the sway.
    vegetation_pipeline: VegetationPipeline,
    wind: Wind,
    wind_binding: WindBinding,
    wind_time: f32,
    /// Navigation demo: a static grid with obstacle cells blocked, a
    /// "walker" cube entity that A*-paths to the player around them, its
    /// current world-space route (XZ), its position, and the last goal it
    /// planned to (re-plans only when the player moves past a threshold).
    nav_grid: NavGrid,
    walker: Entity,
    walker_path: Vec<glam::Vec2>,
    walker_xz: glam::Vec2,
    walker_goal: glam::Vec2,
    skinned_bar: Entity,
    /// The bar's two-joint skeleton and the clip that swings its tip.
    /// Owned here so the per-frame loop can `sample_pose` +
    /// `compute_skinning_matrices` and upload the result.
    bar_skeleton: ImportedSkeleton,
    bar_clip: ImportedAnimation,
    /// Seconds since the bar animation started, wrapped by the clip's
    /// duration each frame.
    bar_anim_elapsed: f32,
    character_controller: CharacterController,
    player_body: RigidBodyHandle,
    player_collider: ColliderHandle,
    /// Accumulated downward speed while airborne — reset to `0.0` on the
    /// frame [`move_character`] reports the player grounded. The engine
    /// has no gravity/input policy of its own for characters (see
    /// [`move_character`]'s docs); this is the example game supplying
    /// one.
    player_vertical_velocity: f32,
    debug_line_pipeline: DebugLinePipeline,
    /// Kept alive for its `Drop` impl — dropping the [`AudioContext`]
    /// tears down the whole audio backend (device stream, every bus,
    /// track, and listener routed through it), not just itself. Playback
    /// now goes through `music_bus`/`sfx_bus`/the spatial track directly,
    /// so this is never read again after setup.
    _audio: AudioContext,
    /// A synthesized "click" WAV, decoded fresh into a new [`StaticSound`]
    /// each play (kira sounds aren't `Clone`-and-replay in this crate's
    /// API yet) — see `sine_wave_wav`.
    click_sfx_wav: Vec<u8>,
    /// Tracks the player's position each frame (see [`PlatformEvent::RedrawRequested`]
    /// handling) — proves the point light's spatial tone actually pans/
    /// attenuates as the listener moves relative to it, not just at a
    /// fixed offset.
    listener: ListenerHandle,
    /// Kept alive for its `Drop` impl (unlike sound handles, dropping a
    /// [`SpatialTrackHandle`] tears the track down) — never read after
    /// setup, so `_`-prefixed.
    _point_light_spatial_track: SpatialTrackHandle,
    /// Bus the background tone plays on — muted/unmuted independently of
    /// everything else via `M` (see [`PlatformEvent::KeyboardInput`]
    /// handling).
    music_bus: Bus,
    music_muted: bool,
    /// Bus the click SFX plays on.
    sfx_bus: Bus,
}

/// Generates a small procedural checkerboard as RGBA8 pixel data.
///
/// Stand-in for a real asset (Milestone 5 adds an asset pipeline) —
/// exercises the upload/sampling path with a pattern where UV mapping
/// errors are immediately visible.
fn checkerboard_rgba(size: u32, squares: u32) -> Vec<u8> {
    let square_size = (size / squares).max(1);
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let is_light = ((x / square_size) + (y / square_size)).is_multiple_of(2);
            let value = if is_light { 220 } else { 60 };
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    pixels
}

/// A tall skinned bar: `cube()` remapped to `x, z` in `[-0.25, 0.25]` and
/// `y` in `[0, 2]`, rigged to two joints — the lower ring to joint 0
/// (static root), the upper ring to joint 1 (bends). Returned with the
/// skeleton those joint indices refer to.
///
/// Stand-in for an imported skinned glTF mesh (loading one through the
/// asset pipeline is a later iteration) — exercises the whole GPU skinning
/// path with geometry whose deformation is easy to eyeball.
fn skinned_bar() -> (Vec<SkinnedVertex>, Vec<u32>, ImportedSkeleton) {
    let (vertices, indices) = cube();
    let skinned_vertices = vertices
        .into_iter()
        .map(|v| {
            // cube y is -0.5 or 0.5 -> remap to 0.0 or 2.0.
            let y = (v.position[1] + 0.5) * 2.0;
            let (joints, weights) = if y < 1.0 {
                ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
            } else {
                ([1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
            };
            SkinnedVertex {
                position: [v.position[0] * 0.5, y, v.position[2] * 0.5],
                normal: v.normal,
                uv: v.uv,
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
                // Inverse of the tip's global bind position (0, 1, 0).
                inverse_bind_matrix: glam::Mat4::from_translation(glam::Vec3::new(0.0, -1.0, 0.0)),
            },
        ],
    };

    (skinned_vertices, indices, skeleton)
}

/// One grass blade: two crossed quads through the origin, base at `y = 0`,
/// tip at `y = 0.5`, ~0.12 wide. The base-at-origin layout is what the
/// vegetation pipeline's wind shader assumes (roots pinned, tips sway).
/// Instanced hundreds of times by a `VegetationRenderer`.
fn grass_blade() -> (Vec<Vertex>, Vec<u32>) {
    let half_width = 0.06_f32;
    let height = 0.5_f32;
    let mut vertices = Vec::with_capacity(8);
    let mut indices = Vec::with_capacity(12);

    // (axis the quad spans, outward normal): one quad across X, one across Z.
    let quads: [(glam::Vec3, [f32; 3]); 2] = [
        (glam::Vec3::X, [0.0, 0.0, 1.0]),
        (glam::Vec3::Z, [1.0, 0.0, 0.0]),
    ];
    for (axis, normal) in quads {
        let edge = axis * half_width;
        let base = vertices.len() as u32;
        let corners = [
            (-edge, 0.0, [0.0, 1.0]),
            (edge, 0.0, [1.0, 1.0]),
            (edge, height, [1.0, 0.0]),
            (-edge, height, [0.0, 0.0]),
        ];
        for (offset, y, uv) in corners {
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

/// A 2-second looping clip that swings [`skinned_bar`]'s tip joint (node
/// index 1) about `Z`, from straight to ~0.9 rad and back.
fn bar_bend_clip() -> ImportedAnimation {
    let mut channels = HashMap::new();
    channels.insert(
        1,
        ImportedAnimationChannels {
            translation: None,
            rotation: Some(ImportedKeyframes {
                interpolation: ImportedInterpolation::Linear,
                times: vec![0.0, 1.0, 2.0],
                values: vec![
                    glam::Quat::IDENTITY,
                    glam::Quat::from_rotation_z(0.9),
                    glam::Quat::IDENTITY,
                ],
            }),
            scale: None,
        },
    );
    ImportedAnimation {
        name: Some("bend".to_string()),
        duration: 2.0,
        channels,
    }
}

/// Synthesizes a short sine tone as a real, valid WAV file (16-bit PCM
/// mono) in memory.
///
/// Stand-in for a real audio asset (no asset pipeline support for audio
/// files beyond raw WAV bytes exists yet) — exercises
/// `StaticSound::from_bytes`'s decode path with a sound whose correctness
/// (pitch, duration) is easy to verify by ear, the same role
/// `checkerboard_rgba` plays for textures. `amplitude` is linear `[0,1]`
/// (not decibels) — kept low for the looping background tone so it's not
/// obnoxious.
fn sine_wave_wav(frequency_hz: f32, duration_secs: f32, amplitude: f32) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    let sample_count = (SAMPLE_RATE as f32 * duration_secs) as u32;
    let data_size = sample_count * 2; // 16-bit mono = 2 bytes/sample.

    let mut wav = Vec::with_capacity(44 + data_size as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM.
    wav.extend_from_slice(&1u16.to_le_bytes()); // Mono.
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // Byte rate.
    wav.extend_from_slice(&2u16.to_le_bytes()); // Block align.
    wav.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample.
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());

    for i in 0..sample_count {
        let t = i as f32 / SAMPLE_RATE as f32;
        let sample = (amplitude
            * (2.0 * std::f32::consts::PI * frequency_hz * t).sin()
            * i16::MAX as f32) as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}

/// [`PlatformHandler`] that stands up the GPU context and draws whatever
/// mesh entities are spawned into its [`Ecs`].
struct GameHandler {
    input: InputState,
    gpu: Option<GpuContext>,
    scene: Option<Scene>,
    /// Toggled by `F1` — draws collider/rigid-body/joint wireframes over
    /// the scene. Off by default (matches typical engine convention: a
    /// debug overlay you opt into, not one imposed on every game).
    debug_render_enabled: bool,
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

        let pipeline = gpu.create_pbr_pipeline("pbr");

        let (vertices, indices) = cube();
        let mesh = match gpu.create_mesh("cube", &vertices, &indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create cube mesh");
                return;
            }
        };

        let aspect_ratio = size.width as f32 / size.height.max(1) as f32;
        let camera = Camera::new(
            glam::Vec3::new(1.5, 1.5, 2.5),
            glam::Vec3::ZERO,
            aspect_ratio,
        );
        let camera_binding = gpu.create_camera_binding(&pipeline, &camera.to_uniform());

        // One sun-like directional light plus one warm point light, so
        // both light types and the BRDF's metallic/roughness response
        // are actually exercised.
        let mut lights = LightSet::new();
        lights.directional.push(DirectionalLight {
            direction: glam::Vec3::new(-0.4, -1.0, -0.3),
            color: glam::Vec3::new(1.0, 0.98, 0.92),
            intensity: 3.0,
        });
        let point_light_position = glam::Vec3::new(-2.0, 1.5, 1.5);
        lights.point.push(PointLight {
            position: point_light_position,
            color: glam::Vec3::new(1.0, 0.6, 0.3),
            intensity: 8.0,
            range: 10.0,
        });
        // The demo's two cubes sit within a radius-3 sphere around the
        // origin — plenty to cover both without wasting shadow-map
        // resolution on empty space.
        let shadow_pipeline = gpu.create_shadow_pipeline(&pipeline, "shadow");
        let light_space_matrix = directional_light_view_projection(
            lights.directional[0].direction,
            glam::Vec3::ZERO,
            3.0,
        );
        let shadow_map = gpu.create_shadow_map(&shadow_pipeline, light_space_matrix);

        let lights_binding =
            gpu.create_lights_binding(&pipeline, &lights.to_uniform(), &shadow_map);

        let sun_direction = lights.directional[0].direction;
        let sun_color = lights.directional[0].color;
        let skybox_pipeline = gpu.create_skybox_pipeline("skybox");
        let skybox_uniform_data = skybox_uniform(&camera, sun_direction, sun_color);
        let skybox = gpu.create_skybox_binding(&skybox_pipeline, &skybox_uniform_data);

        let hdr_target = gpu.create_hdr_target(size.width, size.height);
        // Subtle bloom on, neutral grade, no outline — a calm default that
        // shows the stack works without restyling the sandbox. Tweak at
        // runtime via `scene.post.set_settings(gpu, ...)`.
        let post = gpu.create_post_process_stack(
            &hdr_target,
            size.width,
            size.height,
            PostSettings::default(),
        );

        let debug_line_pipeline = gpu.create_debug_line_pipeline(&pipeline, "debug lines");

        let mut audio = match AudioContext::new() {
            Ok(audio) => audio,
            Err(err) => {
                tracing::error!(error = %err, "failed to initialize audio backend");
                return;
            }
        };
        let click_sfx_wav = sine_wave_wav(880.0, 0.12, 0.4);

        // Separate mixer buses for music and SFX — `M` mutes/unmutes
        // `music_bus` independently of `sfx_bus`, proving `Bus` actually
        // gives each group of sounds its own volume control rather than
        // just routing them to the same place.
        let mut music_bus = match audio.add_bus(0.0) {
            Ok(bus) => bus,
            Err(err) => {
                tracing::error!(error = %err, "failed to create music bus");
                return;
            }
        };
        let sfx_bus = match audio.add_bus(0.0) {
            Ok(bus) => bus,
            Err(err) => {
                tracing::error!(error = %err, "failed to create SFX bus");
                return;
            }
        };

        // Quiet, low, looping "ambient" tone on the music bus — started
        // once here rather than per-frame; its handle isn't kept (kira
        // sounds keep playing independent of their handle's lifetime,
        // they're a remote control, not an RAII guard) since nothing
        // needs to pause/adjust it directly (muting goes through
        // `music_bus` instead).
        match StaticSound::from_bytes(sine_wave_wav(110.0, 4.0, 0.05), true) {
            Ok(music) => {
                if let Err(err) = music.play_on_bus(&mut music_bus) {
                    tracing::warn!(error = %err, "skipped background music playback");
                }
            }
            Err(err) => tracing::warn!(error = %err, "skipped background music decode"),
        }

        let checker_size = 64;
        let texture = gpu.create_texture_from_rgba(
            "checkerboard",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 8),
        );

        // `child`/`ground`/`falling`/`player` below all reuse this same
        // unit-cube mesh via `MeshRenderer::with_mesh` — one GPU upload
        // shared by all five renderers instead of five, demonstrating the
        // dedup an asset-handle-backed `MeshRenderer` buys (see that
        // type's docs). They differ only by `Transform` scale and by their
        // own texture/material.
        let mut render_assets = RenderAssets::new();
        let mut ecs = Ecs::new();

        // Parent cube at the origin; child cube half the size, orbiting
        // out to one side of it — parented via `ChildOf`, so its
        // `GlobalTransform` is the parent's transform composed with its
        // own local one (see `engine_ecs::propagate_transforms`). Each
        // gets a different `base_color_factor` tint, so the PBR material
        // plumbing (factors uniform -> shader) is visibly distinguishable
        // even before real lighting exists to shade metallic/roughness.
        let parent_material = Material {
            base_color_factor: [0.6, 1.0, 0.6, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.8,
        };
        let parent_renderer = ecs_components::MeshRenderer::new(
            &gpu,
            &pipeline,
            &mut render_assets,
            mesh,
            &texture,
            parent_material,
        );
        let cube_mesh = parent_renderer.mesh;
        let parent = ecs
            .world_mut()
            .spawn((ecs_components::Transform::default(), parent_renderer))
            .id();

        let child_texture = gpu.create_texture_from_rgba(
            "checkerboard (child)",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 8),
        );
        let child_material = Material {
            base_color_factor: [1.0, 0.5, 0.5, 1.0],
            metallic_factor: 1.0,
            roughness_factor: 0.3,
        };
        let child_renderer = ecs_components::MeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &child_texture,
            child_material,
        );
        let child_local = Transform::from_translation(glam::Vec3::new(1.5, 0.0, 0.0))
            .with_scale(glam::Vec3::splat(0.5));
        ecs.world_mut().spawn((
            ecs_components::Transform::from(child_local),
            child_renderer,
            ChildOf(parent),
        ));

        // A physics-driven ground plane (fixed body — never moves) and a
        // falling cube (dynamic body — gravity pulls it down until the
        // ground's collider stops it), proving `RigidBody`/`Collider`
        // actually drive rendering through `sync_rigid_bodies`, not just
        // passing unit tests. Positioned to land within the existing
        // camera framing rather than off to the side.
        let mut physics = PhysicsWorld::default();

        let ground_half_extents = glam::Vec3::new(2.5, 0.1, 2.5);
        let ground_texture = gpu.create_texture_from_rgba(
            "ground",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 8),
        );
        let ground_material = Material {
            base_color_factor: [0.7, 0.7, 0.75, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.9,
        };
        let ground_renderer = ecs_components::MeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &ground_texture,
            ground_material,
        );
        let ground_translation = glam::Vec3::new(0.0, -1.0, 0.0);
        let (ground_body, ground_collider) = ecs_components::RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::fixed().translation(ground_translation),
            ColliderBuilder::cuboid(
                ground_half_extents.x,
                ground_half_extents.y,
                ground_half_extents.z,
            ),
        );
        ecs.world_mut().spawn((
            ecs_components::Transform::from(
                Transform::from_translation(ground_translation)
                    .with_scale(ground_half_extents * 2.0),
            ),
            ground_renderer,
            ground_body,
            ground_collider,
        ));

        let falling_half_extent = 0.5_f32;
        let falling_texture = gpu.create_texture_from_rgba(
            "falling cube",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 8),
        );
        let falling_material = Material {
            base_color_factor: [0.4, 0.6, 1.0, 1.0],
            metallic_factor: 0.2,
            roughness_factor: 0.5,
        };
        let falling_renderer = ecs_components::MeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &falling_texture,
            falling_material,
        );
        let falling_start = glam::Vec3::new(-1.5, 2.5, 0.0);
        let (falling_body, falling_collider) = ecs_components::RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic().translation(falling_start),
            ColliderBuilder::cuboid(
                falling_half_extent,
                falling_half_extent,
                falling_half_extent,
            ),
        );
        ecs.world_mut().spawn((
            // Overwritten by `sync_rigid_bodies` before the first render —
            // set to the body's actual starting pose anyway so the ECS
            // state is never inconsistent with `physics`, even for one
            // frame.
            ecs_components::Transform::from(Transform::from_translation(falling_start)),
            falling_renderer,
            falling_body,
            falling_collider,
        ));

        // A WASD-controlled kinematic character (capsule collider; a
        // scaled cube stands in for it visually — no capsule mesh
        // primitive exists yet), proving `move_character` actually
        // resolves collisions against the rest of the scene, not just
        // unit-test geometry. `physics.step` must run at least once
        // before `move_character` can see the colliders spawned above
        // (see its docs) — the first `RedrawRequested` handles that.
        let player_texture = gpu.create_texture_from_rgba(
            "player",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 8),
        );
        let player_material = Material {
            base_color_factor: [0.9, 0.9, 0.3, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.6,
        };
        let player_renderer = ecs_components::MeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &player_texture,
            player_material,
        );
        // Capsule half-height 0.5 + radius 0.3 = 0.8 below/above center;
        // resting on the ground plane's top surface (y = -0.9).
        let player_start = glam::Vec3::new(0.8, -0.1, 1.5);
        let (player_rigid_body, player_collider_component) = ecs_components::RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::kinematic_position_based().translation(player_start),
            ColliderBuilder::capsule_y(0.5, 0.3),
        );
        // `move_character` needs the raw handles directly each frame, not
        // the ECS component wrappers (which just carry the same handles).
        let player_body = player_rigid_body.0;
        let player_collider = player_collider_component.0;
        ecs.world_mut().spawn((
            ecs_components::Transform::from(
                Transform::from_translation(player_start)
                    .with_scale(glam::Vec3::new(0.6, 1.6, 0.6)),
            ),
            player_renderer,
            player_rigid_body,
            player_collider_component,
        ));

        // A GPU-skinned bar, rigged to two joints and driven by a
        // hand-built clip — the top half bends back and forth about its
        // mid hinge, proving `compute_skinning_matrices` -> joint-matrix
        // uniform -> skinning vertex shader deforms real geometry on
        // screen. Off to the side, standing on the ground plane.
        let skinned_pipeline = gpu.create_skinned_pbr_pipeline(&pipeline, "skinned pbr");
        let (bar_vertices, bar_indices, bar_skeleton) = skinned_bar();
        let bar_mesh = match gpu.create_skinned_mesh("bar", &bar_vertices, &bar_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create skinned bar mesh");
                return;
            }
        };
        let bar_texture = gpu.create_texture_from_rgba(
            "skinned bar",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 4),
        );
        let bar_material = Material {
            base_color_factor: [0.8, 0.7, 1.0, 1.0],
            metallic_factor: 0.1,
            roughness_factor: 0.5,
        };
        let bar_renderer = ecs_components::SkinnedMeshRenderer::new(
            &gpu,
            &pipeline,
            &skinned_pipeline,
            &mut render_assets,
            bar_mesh,
            &bar_texture,
            bar_material,
        );
        let skinned_bar = ecs
            .world_mut()
            .spawn((
                ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
                    -1.6, -0.9, -1.0,
                ))),
                bar_renderer,
            ))
            .id();
        let bar_clip = bar_bend_clip();

        // Validation Game 3's instancing + culling piece: an 8x8 grid of
        // 64 cubes drawn in ONE `draw_indexed(.., 0..N)` via
        // `InstancedMeshRenderer`, spread wide enough (x,z in [-14, 14])
        // that a good chunk sits outside the camera frustum — so
        // `RenderStats.instances_drawn` stays well below `instances_total`
        // (logged at TRACE each frame).
        let instanced_pipeline = gpu.create_instanced_pbr_pipeline(&pipeline, "instanced pbr");
        let grid_texture = gpu.create_texture_from_rgba(
            "instanced grid",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 2),
        );
        let grid_material = Material {
            base_color_factor: [0.5, 0.85, 1.0, 1.0],
            metallic_factor: 0.0,
            roughness_factor: 0.7,
        };
        let mut grid_instances = Vec::with_capacity(64);
        for gx in 0..8 {
            for gz in 0..8 {
                let x = (gx as f32 - 3.5) * 4.0;
                let z = (gz as f32 - 3.5) * 4.0;
                grid_instances.push(
                    Transform::from_translation(glam::Vec3::new(x, -0.7, z))
                        .with_scale(glam::Vec3::splat(0.4)),
                );
            }
        }
        let grid_renderer = ecs_components::InstancedMeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &grid_texture,
            grid_material,
            grid_instances,
        );
        ecs.world_mut().spawn(grid_renderer);

        // Stage 5 "Terrain (heightmap) + sculpt" validation: a 96x96
        // heightmap patch with gentle rolling relief, uploaded as a
        // *dynamic* mesh (COPY_DST vertex buffer) and drawn through the
        // ordinary MeshRenderer path. `[` / `]` / `\` sculpt it live in
        // the RedrawRequested handler.
        let terrain_size = 24.0_f32;
        let terrain_resolution = 96_u32;
        let terrain_heightmap =
            match Heightmap::from_fn(terrain_resolution, terrain_size, |row, col| {
                let u = col as f32 / (terrain_resolution - 1) as f32;
                let v = row as f32 / (terrain_resolution - 1) as f32;
                ((u * std::f32::consts::TAU * 1.5).sin() + (v * std::f32::consts::TAU * 1.2).cos())
                    * 0.35
            }) {
                Ok(heightmap) => heightmap,
                Err(err) => {
                    tracing::error!(error = %err, "failed to build terrain heightmap");
                    return;
                }
            };
        let (terrain_vertices, terrain_indices) = terrain_heightmap.mesh_data();
        let terrain_gpu_mesh =
            match gpu.create_mesh_dynamic("terrain", &terrain_vertices, &terrain_indices) {
                Ok(mesh) => mesh,
                Err(err) => {
                    tracing::error!(error = %err, "failed to upload terrain mesh");
                    return;
                }
            };
        let terrain_mesh = render_assets.meshes.insert(terrain_gpu_mesh);
        let terrain_texture = gpu.create_texture_from_rgba(
            "terrain",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 6),
        );
        let terrain_material_binding = gpu.create_material_binding(
            &pipeline,
            &terrain_texture,
            &Material {
                base_color_factor: [0.42, 0.58, 0.33, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.95,
            }
            .into(),
        );
        let terrain_material = render_assets.materials.insert(terrain_material_binding);
        let terrain_renderer = ecs_components::MeshRenderer::from_handles(
            &gpu,
            &pipeline,
            &mut render_assets,
            terrain_mesh,
            terrain_material,
        );
        ecs.world_mut().spawn((
            ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
                0.0, -1.3, 0.0,
            ))),
            terrain_renderer,
        ));

        // Obstacle footprints, shared by the vegetation scatter (keep-out
        // circles) and the nav grid (blocked cells) below.
        let obstacle_positions = [
            glam::Vec2::new(-4.0, -2.0),
            glam::Vec2::new(-1.0, 3.0),
            glam::Vec2::new(3.0, -3.0),
            glam::Vec2::new(4.5, 2.5),
            glam::Vec2::new(0.0, -6.0),
        ];
        // Terrain entity sits at y = -1.3; its heightmap's local origin is
        // world (0, 0) on XZ.
        const TERRAIN_Y_OFFSET: f32 = -1.3;

        // Stage 5 "Procedural scattering" + "Vegetation rendering": grass
        // placed by `scatter` over the terrain — a jittered grid, rejected
        // on slopes steeper than ~32 degrees and inside the obstacle
        // circles — then drawn in one instanced draw through the wind
        // pipeline (blades bend under `Wind::BREEZE`).
        let vegetation_pipeline = gpu.create_vegetation_pipeline(&pipeline, "vegetation");
        let wind = Wind::BREEZE;
        let wind_binding = gpu.create_wind_binding(&vegetation_pipeline, &wind, 0.0);

        let (grass_vertices, grass_indices) = grass_blade();
        let grass_gpu_mesh = match gpu.create_mesh("grass blade", &grass_vertices, &grass_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to upload grass mesh");
                return;
            }
        };
        let grass_texture = gpu.create_texture_from_rgba(
            "grass",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 3),
        );

        let mut grass_config = ScatterConfig::new(
            ScatterArea::new(glam::Vec2::splat(-9.0), glam::Vec2::splat(18.0)),
            0.7,
        );
        grass_config.scale_range = (0.7, 1.4);
        grass_config.slope_limit = Some(32.0_f32.to_radians());
        grass_config.terrain_y_offset = TERRAIN_Y_OFFSET;
        grass_config.exclusions = obstacle_positions.iter().map(|p| (*p, 1.4)).collect();
        let blades = match scatter(&grass_config, 0x51D3_C0DE, Some(&terrain_heightmap)) {
            Ok(placements) => placements,
            Err(err) => {
                tracing::error!(error = %err, "failed to scatter grass");
                return;
            }
        };
        tracing::info!(count = blades.len(), "scattered grass");
        let vegetation_renderer = ecs_components::VegetationRenderer::new(
            &gpu,
            &pipeline,
            &mut render_assets,
            grass_gpu_mesh,
            &grass_texture,
            Material {
                base_color_factor: [0.35, 0.7, 0.28, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 1.0,
            },
            blades,
        );
        ecs.world_mut().spawn(vegetation_renderer);

        // A sparser scatter of "rocks" (scaled cubes) — larger, allowed on
        // steeper ground, and tilted to sit flush with the slope. Drawn as
        // a plain `InstancedMeshRenderer`, showing one scatter feeding both
        // render paths.
        let mut rock_config = ScatterConfig::new(
            ScatterArea::new(glam::Vec2::splat(-9.0), glam::Vec2::splat(18.0)),
            2.6,
        );
        rock_config.scale_range = (0.25, 0.6);
        rock_config.density = 0.7;
        rock_config.align_to_normal = 0.7;
        rock_config.slope_limit = Some(60.0_f32.to_radians());
        rock_config.terrain_y_offset = TERRAIN_Y_OFFSET + 0.15;
        rock_config.exclusions = obstacle_positions.iter().map(|p| (*p, 1.6)).collect();
        let rocks = match scatter(&rock_config, 0x2E57_9A2B, Some(&terrain_heightmap)) {
            Ok(placements) => placements,
            Err(err) => {
                tracing::error!(error = %err, "failed to scatter rocks");
                return;
            }
        };
        tracing::info!(count = rocks.len(), "scattered rocks");
        let rock_texture = gpu.create_texture_from_rgba(
            "rock",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 2),
        );
        let rock_renderer = ecs_components::InstancedMeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &rock_texture,
            Material {
                base_color_factor: [0.5, 0.5, 0.52, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.9,
            },
            rocks,
        );
        ecs.world_mut().spawn(rock_renderer);

        // Stage 5 "Navigation/pathfinding" validation: a 40x40 nav grid
        // (0.5 unit cells) over the play area, five obstacle cubes with
        // their footprints blocked, and a "walker" cube that A*-paths to
        // the player around them each time the player moves far enough.
        // The route is drawn as debug lines.
        let mut nav_grid = match NavGrid::new(40, 40, 0.5, glam::Vec2::new(-10.0, -10.0)) {
            Ok(grid) => grid,
            Err(err) => {
                tracing::error!(error = %err, "failed to build nav grid");
                return;
            }
        };
        let obstacle_texture = gpu.create_texture_from_rgba(
            "obstacle",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 4),
        );
        let obstacle_material = gpu.create_material_binding(
            &pipeline,
            &obstacle_texture,
            &Material {
                base_color_factor: [0.55, 0.4, 0.35, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.85,
            }
            .into(),
        );
        let obstacle_material_handle = render_assets.materials.insert(obstacle_material);
        for position in obstacle_positions {
            nav_grid.block_circle(position, 1.1);
            let obstacle_renderer = ecs_components::MeshRenderer::from_handles(
                &gpu,
                &pipeline,
                &mut render_assets,
                cube_mesh,
                obstacle_material_handle,
            );
            ecs.world_mut().spawn((
                ecs_components::Transform::from(
                    Transform::from_translation(glam::Vec3::new(position.x, WALKER_Y, position.y))
                        .with_scale(glam::Vec3::new(1.6, 1.2, 1.6)),
                ),
                obstacle_renderer,
            ));
        }

        let walker_start = glam::Vec2::new(-8.5, -8.5);
        let walker_texture = gpu.create_texture_from_rgba(
            "walker",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 2),
        );
        let walker_renderer = ecs_components::MeshRenderer::with_mesh(
            &gpu,
            &pipeline,
            &mut render_assets,
            cube_mesh,
            &walker_texture,
            Material {
                base_color_factor: [0.2, 0.85, 0.95, 1.0],
                metallic_factor: 0.1,
                roughness_factor: 0.4,
            },
        );
        let walker = ecs
            .world_mut()
            .spawn((
                ecs_components::Transform::from(
                    Transform::from_translation(glam::Vec3::new(
                        walker_start.x,
                        WALKER_Y,
                        walker_start.y,
                    ))
                    .with_scale(glam::Vec3::splat(0.4)),
                ),
                walker_renderer,
            ))
            .id();

        // Stage 5 "Particle system" validation: an additive ember
        // fountain above the scene. It rises, drifts, and fades — and
        // because it draws into the HDR target inside the scene pass, the
        // post-processing bloom picks up the bright cores (Stage 5's
        // first feature). CPU-simulated by `update_particles` each frame,
        // deterministic from its config seed.
        let particle_pipelines = gpu.create_particle_pipelines("particles");
        let particle_camera = gpu.create_particle_camera_binding(&particle_pipelines, &camera);
        ecs.world_mut().spawn((
            ecs_components::Transform::from(Transform::from_translation(glam::Vec3::new(
                -0.2, -0.5, 0.2,
            ))),
            ParticleEmitter::new(ParticleEmitterConfig {
                // Aim the cone up and slightly toward the camera so the
                // fountain reads clearly in the fixed framing.
                direction: glam::Vec3::new(0.15, 1.0, 0.25),
                ..ParticleEmitterConfig::EMBERS
            }),
        ));

        // Stage 5 "GPU batching" validation: 10 small cubes that all share
        // ONE mesh handle (`cube_mesh`) and ONE material handle — spawned
        // as plain `MeshRenderer`s, no `InstancedMeshRenderer`.
        // `extract_and_render` detects the shared (mesh, material) among
        // the frustum-visible ones and folds them into a single instanced
        // draw automatically. `RenderStats.auto_batches` /
        // `auto_batched_drawn` (logged at TRACE) report it; the folded
        // count drops as the row's ends leave the frustum.
        let batched_texture = gpu.create_texture_from_rgba(
            "batched cubes",
            checker_size,
            checker_size,
            &checkerboard_rgba(checker_size, 4),
        );
        let batched_material_binding = gpu.create_material_binding(
            &pipeline,
            &batched_texture,
            &Material {
                base_color_factor: [1.0, 0.8, 0.4, 1.0],
                metallic_factor: 0.1,
                roughness_factor: 0.6,
            }
            .into(),
        );
        let batched_material = render_assets.materials.insert(batched_material_binding);
        // A short row straddling the origin, inside the camera framing
        // (eye at (1.5, 1.5, 2.5) looking at ZERO) so all ten sit in the
        // frustum and fold into one instanced draw: TRACE shows
        // `auto_batches=1 auto_batched_drawn=10`. Each is still culled
        // individually before batching, so a member leaving view simply
        // drops out of the group.
        for i in 0..10 {
            let x = (i as f32 - 4.5) * 0.34;
            let batched_renderer = ecs_components::MeshRenderer::from_handles(
                &gpu,
                &pipeline,
                &mut render_assets,
                cube_mesh,
                batched_material,
            );
            ecs.world_mut().spawn((
                ecs_components::Transform::from(
                    Transform::from_translation(glam::Vec3::new(x, 0.05, 0.6))
                        .with_scale(glam::Vec3::splat(0.16)),
                ),
                batched_renderer,
            ));
        }

        // A listener at the player's position, plus a spatial track at
        // the point light's position playing a looping tone — walking
        // (WASD) toward/away from and around the light pans and
        // attenuates it, proving `add_listener`/`add_spatial_track`
        // actually drive kira's 3D mixing, not just construct handles.
        let listener = match audio.add_listener(player_start, glam::Quat::IDENTITY) {
            Ok(listener) => listener,
            Err(err) => {
                tracing::error!(error = %err, "failed to create audio listener");
                return;
            }
        };
        let mut point_light_spatial_track =
            match audio.add_spatial_track(&listener, point_light_position) {
                Ok(track) => track,
                Err(err) => {
                    tracing::error!(error = %err, "failed to create spatial audio track");
                    return;
                }
            };
        match StaticSound::from_bytes(sine_wave_wav(440.0, 2.0, 0.5), true) {
            Ok(spatial_tone) => {
                if let Err(err) = spatial_tone.play_on(&mut point_light_spatial_track) {
                    tracing::warn!(error = %err, "skipped spatial tone playback");
                }
            }
            Err(err) => tracing::warn!(error = %err, "skipped spatial tone decode"),
        }

        self.scene = Some(Scene {
            render_assets,
            pipeline,
            shadow_pipeline,
            shadow_map,
            skybox_pipeline,
            skybox,
            hdr_target,
            post,
            sun_direction,
            sun_color,
            camera,
            camera_binding,
            lights_binding,
            ecs,
            physics,
            last_frame: std::time::Instant::now(),
            fixed_timestep: FixedTimestep::default(),
            skinned_pipeline,
            instanced_pipeline,
            particle_pipelines,
            particle_camera,
            terrain_heightmap,
            terrain_mesh,
            vegetation_pipeline,
            wind,
            wind_binding,
            wind_time: 0.0,
            nav_grid,
            walker,
            walker_path: Vec::new(),
            walker_xz: walker_start,
            walker_goal: glam::Vec2::splat(f32::INFINITY),
            skinned_bar,
            bar_skeleton,
            bar_clip,
            bar_anim_elapsed: 0.0,
            character_controller: CharacterController::default(),
            player_body,
            player_collider,
            player_vertical_velocity: 0.0,
            debug_line_pipeline,
            _audio: audio,
            click_sfx_wav,
            listener,
            _point_light_spatial_track: point_light_spatial_track,
            music_bus,
            music_muted: false,
            sfx_bus,
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
                tracing::info!(width, height, "window resized");
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
                    // The skybox's inverse view-projection depends on the
                    // camera's aspect ratio too — keep it in sync.
                    gpu.write_uniform_buffer(
                        &scene.skybox.buffer,
                        &skybox_uniform(&scene.camera, scene.sun_direction, scene.sun_color),
                    );
                    // The HDR target and the post-processing stack (its
                    // half-res bloom targets, and its bind groups pointing
                    // at the HDR view) are all window-sized, so both are
                    // rebuilt rather than rewritten. The stack carries its
                    // current settings forward.
                    scene.hdr_target = gpu.create_hdr_target(width, height);
                    scene.post = gpu.create_post_process_stack(
                        &scene.hdr_target,
                        width,
                        height,
                        scene.post.settings(),
                    );
                }
            }
            PlatformEvent::KeyboardInput {
                key,
                pressed: true,
                repeat: false,
            } => {
                tracing::info!(?key, "key pressed");
                if key == KeyCode::F1 {
                    self.debug_render_enabled = !self.debug_render_enabled;
                    tracing::info!(
                        enabled = self.debug_render_enabled,
                        "physics debug render toggled"
                    );
                }
                // M: mute/unmute the music bus specifically (the point
                // light's spatial tone and click SFX are on separate
                // routing, unaffected) — proves `Bus::set_volume` drives
                // real volume changes, not just that a bus can be
                // created and sounds routed onto it.
                if key == KeyCode::KeyM
                    && let Some(scene) = &mut self.scene
                {
                    scene.music_muted = !scene.music_muted;
                    let volume_decibels = if scene.music_muted { -60.0 } else { 0.0 };
                    scene
                        .music_bus
                        .set_volume(volume_decibels, Tween::default());
                    tracing::info!(muted = scene.music_muted, "music bus toggled");
                }
            }
            PlatformEvent::MouseButtonInput {
                button,
                pressed: true,
            } => {
                tracing::info!(?button, "mouse button pressed");
                // Left click: cast a ray straight down from the player
                // (proving `cast_ray` actually finds real scene geometry,
                // not just unit-test shapes — no debug-draw of the hit,
                // that's "Physics debug rendering"'s job, so the result
                // is logged rather than drawn) and play a click SFX
                // (proving `AudioContext::play_static` actually reaches
                // the audio device).
                if button == MouseButton::Left
                    && let Some(scene) = &mut self.scene
                {
                    if let Some(body) = scene.physics.rapier.bodies.get(scene.player_body) {
                        let origin = body.translation();
                        match cast_ray(
                            &scene.physics,
                            origin,
                            glam::Vec3::NEG_Y,
                            10.0,
                            Some(scene.player_collider),
                        ) {
                            Ok(Some(hit)) => tracing::info!(
                                collider = ?hit.collider,
                                distance = hit.distance,
                                point = ?hit.point,
                                "ray hit"
                            ),
                            Ok(None) => tracing::info!("ray hit nothing within range"),
                            Err(err) => tracing::warn!(error = %err, "skipped ray cast"),
                        }
                    }

                    match StaticSound::from_bytes(scene.click_sfx_wav.clone(), false) {
                        Ok(sound) => {
                            if let Err(err) = sound.play_on_bus(&mut scene.sfx_bus) {
                                tracing::warn!(error = %err, "skipped click SFX playback");
                            }
                        }
                        Err(err) => tracing::warn!(error = %err, "skipped click SFX decode"),
                    }
                }
            }
            PlatformEvent::RedrawRequested => {
                let (Some(gpu), Some(scene)) = (&self.gpu, &mut self.scene) else {
                    return;
                };

                let now = std::time::Instant::now();
                // Raw, unclamped: `fixed_timestep` absorbs a long stall
                // (window occluded, process paused) by capping how many
                // steps it hands back, so a slow frame makes the sim fall
                // behind wall-clock rather than taking one giant step a
                // fast body could tunnel through.
                let frame_seconds = now.duration_since(scene.last_frame).as_secs_f32();
                scene.last_frame = now;

                // WASD-driven horizontal movement plus simple accumulated
                // gravity (reset to zero once grounded) — the engine has
                // no gravity/input policy of its own for characters (see
                // `move_character`'s docs); this is the example game
                // supplying one.
                const MOVE_SPEED: f32 = 3.0;
                const GRAVITY: f32 = -9.81;
                let mut horizontal = glam::Vec3::ZERO;
                if self.input.is_key_held(KeyCode::KeyW) {
                    horizontal.z -= 1.0;
                }
                if self.input.is_key_held(KeyCode::KeyS) {
                    horizontal.z += 1.0;
                }
                if self.input.is_key_held(KeyCode::KeyA) {
                    horizontal.x -= 1.0;
                }
                if self.input.is_key_held(KeyCode::KeyD) {
                    horizontal.x += 1.0;
                }

                // Everything that integrates over time runs once per fixed
                // step, at the same `step` length, so it stays in lockstep
                // with the physics solver regardless of frame rate. Zero
                // steps on a fast frame is fine — rendering below still
                // runs every frame.
                let step = scene.fixed_timestep.step_seconds();
                let horizontal_step = horizontal.normalize_or_zero() * MOVE_SPEED * step;
                for _ in 0..scene.fixed_timestep.advance(frame_seconds) {
                    scene.player_vertical_velocity += GRAVITY * step;
                    let desired_translation = horizontal_step
                        + glam::Vec3::new(0.0, scene.player_vertical_velocity * step, 0.0);
                    match move_character(
                        &mut scene.physics,
                        &scene.character_controller,
                        scene.player_body,
                        scene.player_collider,
                        desired_translation,
                        step,
                    ) {
                        Ok(movement) if movement.grounded => scene.player_vertical_velocity = 0.0,
                        Ok(_) => {}
                        Err(err) => {
                            tracing::warn!(error = %err, "skipped character movement")
                        }
                    }

                    if let Err(err) = scene.physics.step(step) {
                        tracing::warn!(error = %err, "skipped physics step");
                    }
                }
                self.input.end_frame();

                engine::ecs::sync_rigid_bodies(scene.ecs.world_mut(), &scene.physics);

                // Advance every ParticleEmitter with the real frame time,
                // clamped so a long stall (occluded window) doesn't teleport
                // particles. Cosmetic, so it runs once per frame rather than
                // per fixed step.
                let dt = frame_seconds.min(0.1);
                update_particles(scene.ecs.world_mut(), dt);

                // Advance the wind clock (kept bounded so `sin` stays
                // precise over a long session) and push it to the GPU.
                scene.wind_time = (scene.wind_time + dt) % 1000.0;
                gpu.write_wind(&scene.wind_binding, &scene.wind, scene.wind_time);

                // Terrain sculpting: a brush centred on the player (the
                // terrain entity sits at the origin on X/Z). Hold `]` to
                // raise, `[` to lower, `\` to smooth; any edit re-uploads
                // the whole terrain mesh in place.
                {
                    let brush_center = scene
                        .physics
                        .rapier
                        .bodies
                        .get(scene.player_body)
                        .map(|body| {
                            let t = body.translation();
                            glam::Vec2::new(t.x, t.z)
                        })
                        .unwrap_or(glam::Vec2::ZERO);

                    let mut edited = false;
                    if self.input.is_key_held(KeyCode::BracketRight) {
                        scene.terrain_heightmap.raise_lower(&Brush::new(
                            brush_center,
                            3.0,
                            3.0 * dt,
                        ));
                        edited = true;
                    }
                    if self.input.is_key_held(KeyCode::BracketLeft) {
                        scene.terrain_heightmap.raise_lower(&Brush::new(
                            brush_center,
                            3.0,
                            -3.0 * dt,
                        ));
                        edited = true;
                    }
                    if self.input.is_key_held(KeyCode::Backslash) {
                        scene.terrain_heightmap.smooth(&Brush::new(
                            brush_center,
                            3.0,
                            (6.0 * dt).min(1.0),
                        ));
                        edited = true;
                    }
                    if edited {
                        let vertices = scene.terrain_heightmap.vertices();
                        match scene.render_assets.meshes.get_mut(scene.terrain_mesh) {
                            Some(mesh) => {
                                if let Err(err) = gpu.write_mesh_vertices(mesh, &vertices) {
                                    tracing::warn!(error = %err, "skipped terrain re-upload");
                                }
                            }
                            None => tracing::warn!("terrain mesh handle no longer registered"),
                        }
                    }
                }

                // Keep the audio listener glued to the player each frame,
                // so the point light's spatial tone's panning/attenuation
                // actually tracks movement.
                if let Some(body) = scene.physics.rapier.bodies.get(scene.player_body) {
                    scene
                        .listener
                        .set_position(body.translation(), Tween::default());
                }

                // Navigation: re-plan a route from the walker to the
                // player when the player has moved far enough, then step
                // the walker along it.
                {
                    let player_xz = scene
                        .physics
                        .rapier
                        .bodies
                        .get(scene.player_body)
                        .map(|body| {
                            let t = body.translation();
                            glam::Vec2::new(t.x, t.z)
                        })
                        .unwrap_or(glam::Vec2::ZERO);

                    if player_xz.distance(scene.walker_goal) > 0.75 {
                        let start = scene.nav_grid.world_to_coord(scene.walker_xz);
                        let goal = scene.nav_grid.world_to_coord(player_xz);
                        match find_path(&scene.nav_grid, start, goal) {
                            Some(coords) => {
                                let smoothed = smooth_path(&scene.nav_grid, &coords);
                                scene.walker_path = smoothed
                                    .iter()
                                    .map(|c| scene.nav_grid.coord_to_world_center(*c))
                                    .collect();
                            }
                            None => scene.walker_path.clear(),
                        }
                        scene.walker_goal = player_xz;
                    }

                    const WALKER_SPEED: f32 = 2.5;
                    let (new_xz, reached) =
                        follow_path(scene.walker_xz, &scene.walker_path, WALKER_SPEED, dt);
                    scene.walker_xz = new_xz;
                    if reached > 0 {
                        scene
                            .walker_path
                            .drain(..reached.min(scene.walker_path.len()));
                    }
                    if let Some(mut transform) = scene
                        .ecs
                        .world_mut()
                        .get_mut::<ecs_components::Transform>(scene.walker)
                    {
                        transform.0.translation =
                            glam::Vec3::new(scene.walker_xz.x, WALKER_Y, scene.walker_xz.y);
                    }
                }

                // Collider/rigid-body/joint wireframes, only when toggled
                // on (F1) — flattened from `DebugLine { start, end, ... }`
                // pairs into the vertex-per-point layout
                // `DebugLinePipeline` (LineList topology) expects.
                let mut debug_vertices: Vec<DebugLineVertex> = if self.debug_render_enabled {
                    debug_render_lines(
                        &scene.physics,
                        DebugRenderStyle::default(),
                        DebugRenderMode::default(),
                    )
                    .into_iter()
                    .flat_map(|line| {
                        [
                            DebugLineVertex {
                                position: line.start.to_array(),
                                color: line.color,
                            },
                            DebugLineVertex {
                                position: line.end.to_array(),
                                color: line.color,
                            },
                        ]
                    })
                    .collect()
                } else {
                    Vec::new()
                };

                // The walker's current nav route, always drawn: one cyan
                // segment per pair of consecutive waypoints, lifted a
                // little above the walker plane.
                let route_color = [0.2, 0.9, 1.0, 1.0];
                for pair in scene.walker_path.windows(2) {
                    for point in pair {
                        debug_vertices.push(DebugLineVertex {
                            position: [point.x, WALKER_Y + 0.15, point.y],
                            color: route_color,
                        });
                    }
                }

                let debug_lines = (!debug_vertices.is_empty())
                    .then_some((&scene.debug_line_pipeline, debug_vertices.as_slice()));

                // Sample the bar's clip and push fresh skinning matrices to
                // its joint buffer before rendering. This is the caller's
                // job — `engine_ecs` has no animation dependency.
                scene.bar_anim_elapsed += frame_seconds;
                let clip_time = if scene.bar_clip.duration > 0.0 {
                    scene.bar_anim_elapsed % scene.bar_clip.duration
                } else {
                    0.0
                };
                let bar_pose = sample_pose(&scene.bar_skeleton, &scene.bar_clip, clip_time);
                let bar_matrices = compute_skinning_matrices(&scene.bar_skeleton, &bar_pose);
                if let Some(bar) = scene
                    .ecs
                    .world()
                    .get::<ecs_components::SkinnedMeshRenderer>(scene.skinned_bar)
                {
                    gpu.write_uniform_buffer(
                        &bar.skin.joints_buffer,
                        &JointMatricesUniform::from_matrices(&bar_matrices),
                    );
                }

                // Rebuilt each frame from the current camera pose — meshes
                // whose world-space bounds fall outside it are dropped from
                // the scene pass (still drawn into the shadow map).
                let frustum = scene.camera.frustum();

                // Collect this frame's live particles (split by blend mode)
                // and refresh the billboard-basis uniform for the current
                // camera. The slices live here so they outlive the render
                // call that uploads them.
                let (alpha_particles, additive_particles) =
                    engine::ecs::extract_particles(scene.ecs.world_mut());
                gpu.write_particle_camera_binding(&scene.particle_camera, &scene.camera);
                let particle_frame = ParticleFrame {
                    pipelines: &scene.particle_pipelines,
                    camera: &scene.particle_camera,
                    alpha: &alpha_particles,
                    additive: &additive_particles,
                };
                let particles = (!particle_frame.is_empty()).then_some(particle_frame);

                match engine::ecs::extract_and_render(
                    scene.ecs.world_mut(),
                    &scene.render_assets,
                    gpu,
                    &scene.pipeline,
                    &scene.shadow_pipeline,
                    &scene.shadow_map,
                    &scene.skybox_pipeline,
                    &scene.skybox,
                    &scene.hdr_target,
                    &scene.post,
                    &scene.camera_binding,
                    &frustum,
                    &scene.lights_binding,
                    &scene.skinned_pipeline,
                    &scene.instanced_pipeline,
                    debug_lines,
                    particles,
                    Some((&scene.vegetation_pipeline, &scene.wind_binding)),
                ) {
                    Ok(stats) => tracing::trace!(
                        total = stats.meshes_total,
                        drawn = stats.meshes_drawn,
                        skinned = stats.skinned_drawn,
                        instances_total = stats.instances_total,
                        instances_drawn = stats.instances_drawn,
                        auto_batches = stats.auto_batches,
                        auto_batched_drawn = stats.auto_batched_drawn,
                        particles = alpha_particles.len() + additive_particles.len(),
                        vegetation = stats.vegetation_drawn,
                        "frame rendered"
                    ),
                    Err(err) => tracing::error!(error = %err, "frame render failed"),
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

    let config = EngineConfig::new("VGE Example Game", env!("CARGO_PKG_VERSION"));
    let mut app = App::new(config)?;

    let window_config = WindowConfig::new(app.config().app_name.clone(), 1280, 720);
    run_windowed(
        window_config,
        GameHandler {
            input: InputState::new(),
            gpu: None,
            scene: None,
            debug_render_enabled: false,
        },
    )?;

    app.shutdown()?;
    Ok(())
}
