//! **Island** — the engine's playable vertical slice.
//!
//! A walkable procedural island: rolling terrain, scattered conifers and
//! boulders, a character you steer with WASD, an orbiting third-person
//! camera, physics collision against the world, and positional ambient
//! sound.
//!
//! ```sh
//! cargo run -p island
//! ```
//!
//! WASD moves, mouse-drag orbits, Escape exits.
//!
//! ## Why this exists
//!
//! It is the standing acceptance test for the engine — every remaining
//! engine task should make this better, and is validated against it. A
//! passing unit test says a function works; this says a *game* works.
//!
//! It also links `engine` with `default-features = false`, exactly as a
//! shipped title would, so it can never accidentally depend on the editor
//! or AI crates (NFR-004).
//!
//! ## Known rough edges, and what fixes each
//!
//! These are engine gaps, not oversights here:
//!
//! - **Trees are cones, not leaves.** Foliage needs alpha-cutout
//!   materials — spec task T-15.
//! - **The player is a capsule and does not animate.** No animation
//!   component exists yet — T-07.
//! - **Nothing is written on screen.** No text rendering — T-11/T-12.
//! - **Keyboard only.** No gamepad — T-09.
//! - **Nothing persists.** No save/load — T-14.

mod character;
mod world;

use engine::prelude::*;
use glam::{Vec2, Vec3};

use world::{ISLAND_RADIUS, Placement, Rng};

/// What the island remembers between runs.
///
/// The world itself is regenerated from its seed rather than saved —
/// procedural content is cheaper to rebuild than to store, and it stays
/// identical run to run. Only what the *player* changed goes in here.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Progress {
    /// Where the player stood.
    position: [f32; 3],
    /// Which way they faced.
    facing: f32,
    /// How far they have walked, in footsteps.
    steps: u32,
}

/// What the player can do, independent of which device does it.
///
/// Gameplay never asks "is W held" — it asks for `MoveX`. Binding both a
/// thumbstick and a key pair to the same action is what lets the island
/// play identically on a keyboard and a gamepad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Action {
    /// Strafe. `-1` left, `+1` right.
    MoveX,
    /// Walk. `-1` back, `+1` forward.
    MoveZ,
    /// Orbit the camera horizontally.
    LookX,
    /// Orbit the camera vertically.
    LookY,
    /// Quit.
    Quit,
}

/// Binds every action to both a keyboard and a gamepad source.
fn action_map() -> ActionMap<Action> {
    let mut map = ActionMap::new();
    map.bind_axis(Action::MoveX, AxisBinding::StickX(Stick::Left));
    map.bind_axis(
        Action::MoveX,
        AxisBinding::Keys {
            positive: KeyCode::KeyD,
            negative: KeyCode::KeyA,
        },
    );
    map.bind_axis(Action::MoveZ, AxisBinding::StickY(Stick::Left));
    map.bind_axis(
        Action::MoveZ,
        AxisBinding::Keys {
            positive: KeyCode::KeyW,
            negative: KeyCode::KeyS,
        },
    );
    map.bind_axis(Action::LookX, AxisBinding::StickX(Stick::Right));
    map.bind_axis(Action::LookY, AxisBinding::StickY(Stick::Right));
    map.bind(Action::Quit, Binding::Key(KeyCode::Escape));
    map.bind(Action::Quit, Binding::GamepadButton(GamepadButton::Start));
    map
}

/// How fast the player walks, world units per second.
const WALK_SPEED: f32 = 6.0;

/// Downward acceleration applied to the character controller. Separate
/// from the physics world's own gravity because a kinematic body is moved
/// by us, not integrated by the solver.
const FALL_ACCELERATION: f32 = 18.0;

/// Terminal fall speed, so stepping off a cliff stays controllable.
const MAX_FALL_SPEED: f32 = 30.0;

/// Radians of camera orbit per pixel of mouse drag.
const ORBIT_SENSITIVITY: f32 = 0.006;

/// Radians of camera orbit per second at full stick deflection.
const STICK_ORBIT_SPEED: f32 = 2.4;

/// How many trees and boulders to scatter.
const TREE_COUNT: usize = 110;
const ROCK_COUNT: usize = 55;

/// The player's capsule dimensions.
const PLAYER_HALF_HEIGHT: f32 = 0.6;
const PLAYER_RADIUS: f32 = 0.35;

struct Island {
    /// Device-independent input bindings.
    actions: ActionMap<Action>,
    /// Third-person camera, following the player.
    rig: CameraRig,
    /// The player's kinematic body and its collider.
    player: Option<(RigidBodyHandle, ColliderHandle)>,
    /// Collision resolution for the player capsule.
    controller: CharacterController,
    /// Accumulated downward velocity while airborne.
    fall_speed: f32,
    /// Last cursor position, for orbit dragging.
    last_cursor: Option<Vec2>,
    /// Where the player currently is, mirrored for the camera.
    player_position: Vec3,
    /// Seconds since the last status line.
    since_report: f32,
    /// The animated character entity following the physics capsule.
    body: Option<Entity>,
    /// Emitter retriggered by the walk cycle's footstep events.
    footstep: Option<Entity>,
    /// Which way the character is facing, radians about Y. Kept separate
    /// from the physics body, which is a capsule and has no facing.
    facing: f32,
    /// How many footstep events have fired.
    step_count: u32,
    /// Saved progress waiting to be applied once the world exists.
    pending_restore: Option<Progress>,
    /// HUD labels, rebuilt in place each second.
    hud_position: Option<NodeId>,
    hud_steps: Option<NodeId>,
}

impl Island {
    fn new() -> Self {
        Self {
            actions: action_map(),
            rig: CameraRig::new(Vec3::ZERO),
            player: None,
            controller: CharacterController::default(),
            fall_speed: 0.0,
            last_cursor: None,
            player_position: Vec3::ZERO,
            since_report: 0.0,
            body: None,
            footstep: None,
            facing: 0.0,
            step_count: 0,
            pending_restore: None,
            hud_position: None,
            hud_steps: None,
        }
    }

    /// Reads WASD into a world-space direction, relative to where the
    /// camera is facing, so "forward" always means "away from the
    /// camera".
    fn move_input(&self, input: &InputState) -> Vec3 {
        // One read, whichever device supplied it.
        let local = Vec2::new(
            self.actions.axis(Action::MoveX, input),
            self.actions.axis(Action::MoveZ, input),
        );
        if local.length_squared() < 1e-6 {
            return Vec3::ZERO;
        }
        // Clamped rather than normalised: a half-pushed stick should walk
        // at half speed, while a diagonal key press should not exceed
        // full speed.
        let local = if local.length() > 1.0 {
            local.normalize()
        } else {
            local
        };

        // Camera-relative basis, flattened onto the ground plane.
        let (sin_yaw, cos_yaw) = self.rig.yaw.sin_cos();
        let forward = Vec3::new(-sin_yaw, 0.0, -cos_yaw);
        let right = Vec3::new(cos_yaw, 0.0, -sin_yaw);
        (forward * local.y + right * local.x).normalize_or_zero()
    }
}

impl Game for Island {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        // --- terrain ----------------------------------------------
        let (terrain_vertices, terrain_indices) = world::terrain_mesh();
        let (gw, gh, grass) = world::grass_texture();
        spawn_prop(
            ctx,
            &terrain_vertices,
            &terrain_indices,
            (gw, gh, &grass),
            Transform::IDENTITY,
            Material {
                base_color_factor: [1.0, 1.0, 1.0, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.95,
                ..Material::DEFAULT
            },
        )?;

        // A collider matching the terrain, so the player walks on it
        // rather than through it. A trimesh built from a coarser sample
        // of the same height function: exact against what is drawn,
        // without paying for render-mesh density in every query.
        let (collider_points, collider_indices) = terrain_collider_mesh();
        match ColliderBuilder::trimesh(collider_points, collider_indices) {
            Ok(collider) => {
                ctx.physics_mut()
                    .rapier
                    .insert(RigidBodyBuilder::fixed().translation(Vec3::ZERO), collider);
            }
            Err(err) => {
                // Degenerate terrain geometry would leave the player
                // falling forever; better to say so than to ship a
                // silently bottomless island.
                return Err(GameError::Setup(format!(
                    "terrain collision mesh is invalid: {err}"
                )));
            }
        }

        // --- scattered props --------------------------------------
        let (bw, bh, bark) = world::bark_texture();
        let (fw, fh, foliage) = world::foliage_texture();
        let (sw, sh, stone) = world::stone_texture();

        let mut rng = Rng::new(0xC0FFEE);
        for placement in world::scatter(0xA11CE, TREE_COUNT, 0.5, 0.82) {
            let transform = transform_of(placement);

            // Trunk: opaque bark.
            let (trunk_vertices, trunk_indices) = world::trunk_mesh(&mut rng);
            spawn_prop(
                ctx,
                &trunk_vertices,
                &trunk_indices,
                (bw, bh, &bark),
                transform,
                Material {
                    base_color_factor: [1.0, 1.0, 1.0, 1.0],
                    metallic_factor: 0.0,
                    roughness_factor: 0.9,
                    ..Material::DEFAULT
                },
            )?;

            // Canopy: alpha-cut cards. The texture's alpha channel is the
            // leaf shape; `Material::FOLIAGE` discards everything under
            // half alpha, which is what makes flat quads read as foliage.
            let (canopy_vertices, canopy_indices) = world::canopy_mesh(&mut rng);
            spawn_prop(
                ctx,
                &canopy_vertices,
                &canopy_indices,
                (fw, fh, &foliage),
                transform,
                Material::FOLIAGE,
            )?;

            // A simple upright cylinder is a good enough collider for a
            // tree; nobody should be able to walk through the trunk.
            ctx.physics_mut().rapier.insert(
                RigidBodyBuilder::fixed().translation(placement.position),
                ColliderBuilder::cylinder(1.6 * placement.scale, 0.35 * placement.scale),
            );
        }

        for placement in world::scatter(0xB0B, ROCK_COUNT, 0.2, 0.6) {
            let (vertices, indices) = world::rock_mesh(&mut rng);
            spawn_prop(
                ctx,
                &vertices,
                &indices,
                (sw, sh, &stone),
                transform_of(placement),
                Material {
                    base_color_factor: [1.0, 1.0, 1.0, 1.0],
                    metallic_factor: 0.0,
                    roughness_factor: 0.8,
                    ..Material::DEFAULT
                },
            )?;
            ctx.physics_mut().rapier.insert(
                RigidBodyBuilder::fixed().translation(placement.position),
                ColliderBuilder::ball(0.55 * placement.scale),
            );
        }

        // --- the player -------------------------------------------
        let spawn_height = world::height_at(0.0, 0.0) + 2.0;
        let spawn = Vec3::new(0.0, spawn_height, 0.0);
        let (body, collider) = ctx.physics_mut().rapier.insert(
            RigidBodyBuilder::kinematic_position_based().translation(spawn),
            ColliderBuilder::capsule_y(PLAYER_HALF_HEIGHT, PLAYER_RADIUS),
        );
        self.player = Some((body, collider));
        self.player_position = spawn;
        self.rig = CameraRig::new(spawn);

        // The player's body: a procedurally-skinned character driven by
        // the engine's animation system. Its walk clip is paused when
        // standing still and played while moving (see `update`).
        let (pw, ph, player_skin) = world::flat_texture([214, 122, 96]);
        let (skin_vertices, skin_indices) = character::mesh();
        let skeleton = character::skeleton();
        let clip = character::walk_clip();

        let parts = ctx.scene_parts();
        let skin_texture = parts
            .gpu
            .create_texture_from_rgba("player skin", pw, ph, &player_skin);
        let skinned_mesh = parts
            .gpu
            .create_skinned_mesh("player", &skin_vertices, &skin_indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        let renderer = engine::ecs::components::SkinnedMeshRenderer::new(
            parts.gpu,
            parts.pipeline,
            parts.skinned_pipeline,
            parts.assets,
            skinned_mesh,
            &skin_texture,
            Material {
                base_color_factor: [1.0, 1.0, 1.0, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.75,
                ..Material::DEFAULT
            },
        );

        // Footsteps ride the animation, not a timer, so they stay in sync
        // with the feet at any playback speed.
        let footsteps = character::footstep_times()
            .into_iter()
            .map(|time| AnimationEvent::new(time, "footstep"))
            .collect();

        self.body = Some(
            parts
                .world
                .spawn((
                    engine::ecs::components::Transform::from(Transform::from_translation(spawn)),
                    engine::ecs::components::GlobalTransform::default(),
                    renderer,
                    AnimationPlayer::new(skeleton, clip).with_events(footsteps),
                    AnimationEvents::default(),
                ))
                .id(),
        );

        // --- lighting ---------------------------------------------
        // Warm low sun plus the hemisphere ambient added in T-20. Without
        // the ambient term every shaded face would be near-black.
        let lights = ctx.lights_mut();
        lights.directional.clear();
        lights.directional.push(DirectionalLight {
            direction: Vec3::new(-0.45, -0.78, -0.44).normalize(),
            color: Vec3::new(1.0, 0.94, 0.82),
            intensity: 3.4,
        });
        lights.ambient = AmbientLight {
            sky_color: Vec3::new(0.42, 0.56, 0.74),
            ground_color: Vec3::new(0.28, 0.26, 0.19),
            intensity: 1.0,
        };

        // --- audio ------------------------------------------------
        // The camera hears; a gentle tone marks the island's centre so
        // panning is audible as you walk around it.
        ctx.spawn((
            engine::ecs::components::Transform::from(Transform::from_translation(spawn)),
            engine::ecs::components::GlobalTransform::default(),
            AudioListener::default(),
        ));
        if let Some(sound) = footstep_sound() {
            // Silent until the walk cycle's footstep event triggers it,
            // and parented to nothing — it is moved onto the character
            // each frame so the sound comes from where the feet are.
            self.footstep = Some(ctx.spawn((
                engine::ecs::components::Transform::from(Transform::from_translation(spawn)),
                engine::ecs::components::GlobalTransform::default(),
                AudioEmitter::silent(sound),
            )));
        }
        if !ctx.has_audio() {
            tracing::warn!("no audio device; the island runs silent");
        }

        // --- HUD --------------------------------------------------
        // The first thing this slice has ever shown without a terminal.
        build_hud(self, ctx);

        tracing::info!(
            trees = TREE_COUNT,
            rocks = ROCK_COUNT,
            radius = ISLAND_RADIUS,
            "island generated — WASD to walk, drag to orbit, Escape to quit"
        );
        Ok(())
    }

    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, step: f32) {
        let Some((body, collider)) = self.player else {
            return;
        };

        let direction = self.move_input(ctx.input());
        if direction != Vec3::ZERO {
            // Face the way we are going. `atan2(x, z)` because the model
            // faces -Z at rest, matching the engine's convention.
            self.facing = direction.x.atan2(direction.z);
        }
        self.fall_speed = (self.fall_speed + FALL_ACCELERATION * step).min(MAX_FALL_SPEED);
        let desired = direction * WALK_SPEED * step + Vec3::NEG_Y * self.fall_speed * step;

        match move_character(
            ctx.physics_mut(),
            &self.controller,
            body,
            collider,
            desired,
            step,
        ) {
            Ok(movement) => {
                if movement.grounded {
                    // Reset rather than zero: a tiny downward bias keeps
                    // the capsule pinned to slopes instead of skipping.
                    self.fall_speed = 1.0;
                }
            }
            Err(err) => tracing::warn!(error = %err, "character move rejected"),
        }
    }

    fn save_data(&self) -> Option<String> {
        let progress = Progress {
            position: self.player_position.to_array(),
            facing: self.facing,
            steps: self.step_count,
        };
        match ron::ser::to_string(&progress) {
            Ok(text) => Some(text),
            Err(err) => {
                tracing::warn!(error = %err, "could not encode progress");
                None
            }
        }
    }

    fn load_data(&mut self, data: &str) {
        match ron::from_str::<Progress>(data) {
            Ok(progress) => {
                self.pending_restore = Some(progress);
                tracing::info!("resuming from the last session");
            }
            Err(err) => tracing::warn!(error = %err, "ignoring unreadable progress"),
        }
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        // Applied here rather than in `load_data`, which runs before the
        // physics body exists to move.
        if let Some(progress) = self.pending_restore.take() {
            self.step_count = progress.steps;
            self.facing = progress.facing;
            let position = Vec3::from_array(progress.position);
            if let Some((body, _)) = self.player
                && let Some(rigid_body) = ctx.physics_mut().rapier.bodies.get_mut(body)
            {
                rigid_body.set_next_kinematic_translation(position);
                self.player_position = position;
            }
            tracing::info!(
                x = position.x,
                z = position.z,
                steps = progress.steps,
                "restored"
            );
        }

        if self.actions.is_pressed(Action::Quit, ctx.input()) {
            ctx.request_exit();
        }

        // Orbit while the left button is held.
        let cursor = ctx
            .input()
            .cursor_position()
            .map(|(x, y)| Vec2::new(x as f32, y as f32));
        if ctx.input().is_mouse_button_held(MouseButton::Left)
            && let (Some(current), Some(previous)) = (cursor, self.last_cursor)
        {
            let delta = current - previous;
            self.rig
                .orbit(-delta.x * ORBIT_SENSITIVITY, -delta.y * ORBIT_SENSITIVITY);
        }
        self.last_cursor = cursor;

        // Right stick orbits too. Rate-based rather than delta-based: a
        // held stick should keep turning, unlike a mouse that stops.
        let look = Vec2::new(
            self.actions.axis(Action::LookX, ctx.input()),
            self.actions.axis(Action::LookY, ctx.input()),
        );
        if look.length_squared() > 1e-6 {
            self.rig.orbit(
                -look.x * STICK_ORBIT_SPEED * dt,
                look.y * STICK_ORBIT_SPEED * dt,
            );
        }

        // Follow the player.
        if let Some((body, _)) = self.player
            && let Some(rigid_body) = ctx.physics().rapier.bodies.get(body)
        {
            self.player_position = rigid_body.translation();
        }
        // A periodic status line: without on-screen text (T-11/T-12)
        // this is the only way to see what the player is doing.
        self.since_report += dt;
        if self.since_report >= 2.0 {
            self.since_report = 0.0;
            let ground = world::height_at(self.player_position.x, self.player_position.z);
            let (clip_time, walking) = self
                .body
                .and_then(|body| ctx.world().get::<AnimationPlayer>(body))
                .map_or((0.0, false), |player| (player.time, player.playing));
            // Same numbers the log carries, now on screen too.
            let position_text = format!(
                "pos {:.1}, {:.1}, {:.1}",
                self.player_position.x, self.player_position.y, self.player_position.z
            );
            let steps_text = format!("steps {}", self.step_count);
            if let Some(node) = self.hud_position {
                set_label(ctx.ui_mut(), node, &position_text);
            }
            if let Some(node) = self.hud_steps {
                set_label(ctx.ui_mut(), node, &steps_text);
            }

            tracing::info!(
                x = self.player_position.x,
                y = self.player_position.y,
                z = self.player_position.z,
                ground,
                clip_time,
                walking,
                steps = self.step_count,
                "player"
            );
        }

        // Move the visible character onto the physics capsule, face it
        // the way it is walking, and let the walk cycle run only while
        // there is movement to justify it.
        if let Some(body) = self.body {
            let moving = self.move_input(ctx.input()) != Vec3::ZERO;
            let facing = self.facing;
            let feet = self.player_position - Vec3::Y * (PLAYER_HALF_HEIGHT + PLAYER_RADIUS);

            if let Some(mut transform) = ctx
                .world_mut()
                .get_mut::<engine::ecs::components::Transform>(body)
            {
                transform.0.translation = feet;
                transform.0.rotation = glam::Quat::from_rotation_y(facing);
            }
            if let Some(emitter) = self.footstep
                && let Some(mut transform) = ctx
                    .world_mut()
                    .get_mut::<engine::ecs::components::Transform>(emitter)
            {
                transform.0.translation = feet;
            }
            if let Some(mut player) = ctx.world_mut().get_mut::<AnimationPlayer>(body) {
                // Pausing rather than zeroing speed keeps the pose where
                // it stopped instead of snapping to the bind pose.
                player.playing = moving;
            }

            // Footstep audio driven by the clip's own events, so it stays
            // in sync with the feet at any playback speed.
            let stepped = ctx
                .world()
                .get::<AnimationEvents>(body)
                .is_some_and(|events| events.fired("footstep"));
            if stepped {
                self.step_count += 1;
            }
            if stepped
                && let Some(emitter) = self.footstep
                && let Some(mut emitter) = ctx.world_mut().get_mut::<AudioEmitter>(emitter)
            {
                emitter.play();
            }
        }

        self.rig.target = self.player_position;
        let aspect = ctx.camera_mut().aspect_ratio;
        self.rig.apply(ctx.camera_mut(), dt);
        ctx.camera_mut().aspect_ratio = aspect;
    }
}

/// Builds the HUD: a translucent panel with two readouts and a hint line.
fn build_hud(island: &mut Island, ctx: &mut GameContext<'_>) {
    let text = UiColor {
        r: 0.92,
        g: 0.95,
        b: 1.0,
        a: 1.0,
    };
    let ui = ctx.ui_mut();
    ui.clear();

    let panel = ui.add(
        None,
        UiStyle {
            anchor: Anchor::TopLeft,
            offset: [12.0, 12.0],
            size: [260.0, 92.0],
        },
        UiWidget::Panel {
            color: UiColor {
                r: 0.05,
                g: 0.07,
                b: 0.10,
                a: 0.55,
            },
        },
    );

    let mut label = |island_slot: &mut Option<NodeId>, y: f32, content: &str| {
        let node = ui.add(
            Some(panel),
            UiStyle {
                anchor: Anchor::TopLeft,
                offset: [10.0, y],
                size: [240.0, 18.0],
            },
            UiWidget::Label {
                text: content.to_string(),
                color: text,
            },
        );
        *island_slot = Some(node);
    };

    label(&mut island.hud_position, 8.0, "pos 0.0, 0.0, 0.0");
    label(&mut island.hud_steps, 30.0, "steps 0");

    // A static hint, so the controls are discoverable in-game rather
    // than only in the crate docs.
    let mut hint = None;
    label(&mut hint, 56.0, "WASD / stick to walk");
}

/// Replaces a label node's text in place.
///
/// Cheaper and less disruptive than rebuilding the tree: node ids stay
/// valid, so the HUD keeps its identity across updates.
fn set_label(ui: &mut Ui, node: NodeId, content: &str) {
    if let Some(node) = ui.node_mut(node)
        && let UiWidget::Label { text, .. } = &mut node.widget
        && text != content
    {
        text.clear();
        text.push_str(content);
    }
}

/// The world transform for a scattered prop.
fn transform_of(placement: Placement) -> Transform {
    Transform {
        translation: placement.position,
        rotation: glam::Quat::from_rotation_y(placement.yaw),
        scale: Vec3::splat(placement.scale),
    }
}

/// Spawns a static, textured, lit mesh entity.
fn spawn_prop(
    ctx: &mut GameContext<'_>,
    vertices: &[Vertex],
    indices: &[u32],
    texture: (u32, u32, &[u8]),
    transform: Transform,
    material: Material,
) -> Result<(), GameError> {
    let (width, height, pixels) = texture;
    let parts = ctx.scene_parts();
    let renderer = engine::ecs::components::MeshRenderer::from_geometry(
        parts.gpu,
        parts.pipeline,
        parts.assets,
        vertices,
        indices,
        Some((width, height, pixels)),
        material,
    )
    .map_err(|err| GameError::Setup(err.to_string()))?;
    parts.world.spawn((
        engine::ecs::components::Transform::from(transform),
        engine::ecs::components::GlobalTransform::default(),
        renderer,
    ));
    Ok(())
}

/// Samples the terrain into a collision trimesh.
///
/// Coarser than the render mesh — collision needs the shape, not the
/// silhouette, and a smaller mesh is cheaper to query every frame.
fn terrain_collider_mesh() -> (Vec<Vec3>, Vec<[u32; 3]>) {
    const RESOLUTION: usize = 48;
    let step = (ISLAND_RADIUS * 2.0) / (RESOLUTION - 1) as f32;

    let mut points = Vec::with_capacity(RESOLUTION * RESOLUTION);
    for row in 0..RESOLUTION {
        for column in 0..RESOLUTION {
            let x = -ISLAND_RADIUS + column as f32 * step;
            let z = -ISLAND_RADIUS + row as f32 * step;
            points.push(Vec3::new(x, world::height_at(x, z), z));
        }
    }

    let mut indices = Vec::with_capacity((RESOLUTION - 1) * (RESOLUTION - 1) * 2);
    for row in 0..RESOLUTION - 1 {
        for column in 0..RESOLUTION - 1 {
            let i = (row * RESOLUTION + column) as u32;
            let below = i + RESOLUTION as u32;
            indices.push([i, below, i + 1]);
            indices.push([i + 1, below, below + 1]);
        }
    }

    (points, indices)
}

/// A short, soft footfall — noise burst with a fast decay.
fn footstep_sound() -> Option<StaticSound> {
    const RATE: u32 = 22_050;
    const SECONDS: f32 = 0.16;
    let sample_count = (RATE as f32 * SECONDS) as u32;

    // A deterministic pseudo-noise burst: cheap, and it reads as a
    // footfall rather than a tone.
    let mut noise = world::Rng::new(0x5EED);
    let mut samples = Vec::with_capacity(sample_count as usize * 2);
    for i in 0..sample_count {
        let t = i as f32 / RATE as f32;
        let envelope = (1.0 - t / SECONDS).max(0.0).powi(3);
        // Low-frequency thump under the noise gives it weight.
        let thump = (t * 70.0 * std::f32::consts::TAU).sin() * 0.5;
        let hiss = (noise.unit() * 2.0 - 1.0) * 0.35;
        let value = (thump + hiss) * envelope * 0.5;
        samples.extend_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes());
    }

    let data_len = samples.len() as u32;
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&samples);

    // One-shot: retriggered per footstep, never looped.
    match StaticSound::from_bytes(wav, false) {
        Ok(sound) => Some(sound),
        Err(err) => {
            tracing::warn!(error = %err, "could not build the footstep sound");
            None
        }
    }
}

fn main() -> Result<(), GameError> {
    let _ = logging::init_default();

    let mut config = GameConfig::new("Island", 1280, 720);
    config.camera = Camera::new(Vec3::new(8.0, 6.0, 10.0), Vec3::ZERO, 16.0 / 9.0);
    // Autosave, and resume on launch. No save key: close the window and
    // reopen it, and you are where you left off.
    config.save_path = Some(std::path::PathBuf::from("island-save.ron"));
    config.save_policy = SavePolicy::Continuous { interval: 10.0 };
    run_game(config, Island::new())
}
