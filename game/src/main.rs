//! Example game project for the Vibe Game Engine (VGE).
//!
//! This binary is the engine's first consumer: each milestone extends it
//! to exercise newly implemented engine features.

use std::sync::Arc;

use engine::ecs::components as ecs_components;
use engine::ecs::prelude::ChildOf;
use engine::prelude::*;

/// GPU resources and the ECS world for drawing the example scene.
///
/// The renderer no longer holds a direct reference to "the cube" — it
/// draws whatever `(Transform, MeshRenderer)` entities exist in `ecs`
/// (see `engine_ecs::extract_and_render`). `pipeline`/`camera`/
/// `camera_binding` stay outside the ECS: one shared pipeline and one
/// camera for the whole frame, not per-entity data.
struct Scene {
    pipeline: Pipeline,
    shadow_pipeline: ShadowPipeline,
    shadow_map: ShadowMap,
    skybox_pipeline: SkyboxPipeline,
    skybox: SkyboxBinding,
    hdr_target: HdrTarget,
    tonemap_pipeline: TonemapPipeline,
    tonemap_binding: TonemapBinding,
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
    /// Wall-clock time of the last [`PlatformEvent::RedrawRequested`],
    /// for computing `physics.step`'s per-frame `dt`.
    last_frame: std::time::Instant,
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
        let tonemap_pipeline = gpu.create_tonemap_pipeline("tonemap");
        let tonemap_binding =
            gpu.create_tonemap_binding(&tonemap_pipeline, &hdr_target, DEFAULT_EXPOSURE);

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

        let (child_vertices, child_indices) = cube();
        let child_mesh = match gpu.create_mesh("cube (child)", &child_vertices, &child_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create child cube mesh");
                return;
            }
        };

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
        let parent_renderer =
            ecs_components::MeshRenderer::new(&gpu, &pipeline, mesh, &texture, parent_material);
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
        let child_renderer = ecs_components::MeshRenderer::new(
            &gpu,
            &pipeline,
            child_mesh,
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
        let (ground_vertices, ground_indices) = cube();
        let ground_mesh = match gpu.create_mesh("ground", &ground_vertices, &ground_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create ground mesh");
                return;
            }
        };
        let ground_renderer = ecs_components::MeshRenderer::new(
            &gpu,
            &pipeline,
            ground_mesh,
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
        let (falling_vertices, falling_indices) = cube();
        let falling_mesh =
            match gpu.create_mesh("falling cube", &falling_vertices, &falling_indices) {
                Ok(mesh) => mesh,
                Err(err) => {
                    tracing::error!(error = %err, "failed to create falling cube mesh");
                    return;
                }
            };
        let falling_renderer = ecs_components::MeshRenderer::new(
            &gpu,
            &pipeline,
            falling_mesh,
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
        let (player_vertices, player_indices) = cube();
        let player_mesh = match gpu.create_mesh("player", &player_vertices, &player_indices) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::error!(error = %err, "failed to create player mesh");
                return;
            }
        };
        let player_renderer = ecs_components::MeshRenderer::new(
            &gpu,
            &pipeline,
            player_mesh,
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
            pipeline,
            shadow_pipeline,
            shadow_map,
            skybox_pipeline,
            skybox,
            hdr_target,
            tonemap_pipeline,
            tonemap_binding,
            sun_direction,
            sun_color,
            camera,
            camera_binding,
            lights_binding,
            ecs,
            physics,
            last_frame: std::time::Instant::now(),
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
                    // The HDR target is sized to the window, so it (and
                    // the tonemap bind group pointing at its view) must be
                    // rebuilt rather than just rewritten.
                    scene.hdr_target = gpu.create_hdr_target(width, height);
                    scene.tonemap_binding = gpu.create_tonemap_binding(
                        &scene.tonemap_pipeline,
                        &scene.hdr_target,
                        DEFAULT_EXPOSURE,
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
                // Clamped so a long stall (e.g. the window was occluded or
                // the process was paused by the OS) can't hand physics a
                // huge `dt` — a fast-moving body could tunnel straight
                // through a thin collider instead of colliding with it.
                let dt = now
                    .duration_since(scene.last_frame)
                    .as_secs_f32()
                    .min(1.0 / 20.0);
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
                scene.player_vertical_velocity += GRAVITY * dt;
                let desired_translation = horizontal.normalize_or_zero() * MOVE_SPEED * dt
                    + glam::Vec3::new(0.0, scene.player_vertical_velocity * dt, 0.0);
                match move_character(
                    &mut scene.physics,
                    &scene.character_controller,
                    scene.player_body,
                    scene.player_collider,
                    desired_translation,
                    dt,
                ) {
                    Ok(movement) if movement.grounded => scene.player_vertical_velocity = 0.0,
                    Ok(_) => {}
                    Err(err) => tracing::warn!(error = %err, "skipped character movement"),
                }
                self.input.end_frame();

                if let Err(err) = scene.physics.step(dt) {
                    tracing::warn!(error = %err, "skipped physics step");
                }
                engine::ecs::sync_rigid_bodies(scene.ecs.world_mut(), &scene.physics);

                // Keep the audio listener glued to the player each frame,
                // so the point light's spatial tone's panning/attenuation
                // actually tracks movement.
                if let Some(body) = scene.physics.rapier.bodies.get(scene.player_body) {
                    scene
                        .listener
                        .set_position(body.translation(), Tween::default());
                }

                // Collider/rigid-body/joint wireframes, only when toggled
                // on (F1) — flattened from `DebugLine { start, end, ... }`
                // pairs into the vertex-per-point layout
                // `DebugLinePipeline` (LineList topology) expects.
                let debug_vertices: Vec<DebugLineVertex> = if self.debug_render_enabled {
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
                let debug_lines = (!debug_vertices.is_empty())
                    .then_some((&scene.debug_line_pipeline, debug_vertices.as_slice()));

                if let Err(err) = engine::ecs::extract_and_render(
                    scene.ecs.world_mut(),
                    gpu,
                    &scene.pipeline,
                    &scene.shadow_pipeline,
                    &scene.shadow_map,
                    &scene.skybox_pipeline,
                    &scene.skybox,
                    &scene.hdr_target,
                    &scene.tonemap_pipeline,
                    &scene.tonemap_binding,
                    &scene.camera_binding,
                    &scene.lights_binding,
                    debug_lines,
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
