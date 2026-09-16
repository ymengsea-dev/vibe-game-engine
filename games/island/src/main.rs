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
//! WASD moves, mouse-drag orbits, Escape opens the pause menu
//! (navigable with the arrow keys, the d-pad, or the left stick).
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
//! ## Where the art comes from
//!
//! Everything is procedural, and everything is a **file**. The
//! generators in [`island::world`] run in the baker
//! (`cargo run -p island --bin bake`), which writes real `.png`, `.gltf`
//! and `.wav` assets plus a scene into this project's `assets/` and
//! `scenes/` folders. Open `games/island` in the studio and they are all
//! there to browse.
//!
//! This binary still builds its world in code at startup (T-23 switches
//! it over to loading the baked project). The two paths share the same
//! generators, so they cannot drift apart in the meantime.
//!
//! ## Known rough edges, and what fixes each
//!
//! - **The game builds its world at startup** rather than loading the
//!   project the baker wrote — T-23.
//! - **The character is built in code**, because the glTF writer does
//!   not handle skins or animations yet — T-24.
//! - **Lights, camera and colliders are code**, because the scene format
//!   carries none of them.

use engine::prelude::*;
use engine_project::Project;
use glam::{Vec2, Vec3};
use island::world;

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
    /// Open / close the pause menu.
    Pause,
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
    map.bind(Action::Pause, Binding::Key(KeyCode::Escape));
    map.bind(Action::Pause, Binding::GamepadButton(GamepadButton::Start));
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
    /// Whether the pause menu is open. Gameplay and physics stop while
    /// it is.
    paused: bool,
    /// Pause-menu buttons, valid only while [`Island::paused`].
    menu_resume: Option<NodeId>,
    menu_quit: Option<NodeId>,
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
            paused: false,
            menu_resume: None,
            menu_quit: None,
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
        // --- the project ------------------------------------------
        // Everything the world is made of comes off disk: the scene
        // names its props, the props name their models, the models name
        // their textures. Nothing here generates art — that is the
        // baker's job, and its output is what loads.
        // Two ways to start, and the shipped one comes first: an
        // exported folder (an `assets.pak` and a `main.ron` beside this
        // executable) has no `project.ron` anywhere near it, and a
        // player's machine has no source tree to fall back to.
        let (library, scene) = match executable_dir().as_deref().and_then(open_export) {
            Some(content) => {
                let content = content
                    .map_err(|err| GameError::Setup(format!("opening the export: {err}")))?;
                (content.library, content.scene)
            }
            None => {
                let root = project_root();
                let project = Project::open(&root).map_err(|err| {
                    GameError::Setup(format!("opening {}: {err}", root.display()))
                })?;
                let library = AssetLibrary::index(&project.assets_dir())
                    .map_err(|err| GameError::Setup(format!("indexing assets: {err}")))?;
                let scene = Scene::load_from_file(&project.main_scene_path())
                    .map_err(|err| GameError::Setup(format!("loading the scene: {err}")))?;
                (library, scene)
            }
        };
        scene
            .validate()
            .map_err(|err| GameError::Setup(format!("scene is invalid: {err}")))?;

        let report = {
            let parts = ctx.scene_parts();
            let mut resolver =
                RuntimeResolver::new(&library, parts.gpu, parts.pipeline, parts.assets);
            scene.instantiate_with_resolver(parts.world, &mut resolver)
        };
        if report.unresolved > 0 {
            // Not fatal — the rest of the island still loads — but a
            // silent hole in the world is worse than a loud one.
            tracing::warn!(
                unresolved = report.unresolved,
                "some scene entities could not find their assets"
            );
        }

        // Bodies from the same file, so what you collide with is what
        // you see.
        let bodies = ctx.spawn_scene_colliders(&scene, &report, &library);
        tracing::info!(
            entities = report.spawned.len(),
            bodies,
            assets = library.len(),
            "loaded the island project"
        );

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

        // The player's body, loaded from `character.gltf`: skeleton,
        // skin weights, walk clip and the clip's own footstep events all
        // come out of the file. Spawned by code rather than placed in
        // the scene because the scene format has no skinned-mesh field
        // yet — the asset is data, its placement is not.
        let character_ref = AssetRef {
            id: library
                .id_for_path("models/character.gltf")
                .ok_or_else(|| GameError::Setup("the project has no character model".to_string()))?
                .to_string(),
        };
        let character = load_skinned_mesh(&library, &character_ref)
            .map_err(|err| GameError::Setup(format!("loading the character: {err}")))?
            .ok_or_else(|| GameError::Setup("the character model is missing".to_string()))?;
        let clip = character
            .animation("walk")
            .ok_or_else(|| GameError::Setup("the character has no walk clip".to_string()))?
            .clone();
        // Events ride the clip, so they stay in sync with the feet at
        // any playback speed — and are retimed by re-baking, not by
        // editing this file.
        let footsteps: Vec<AnimationEvent> = clip
            .events
            .iter()
            .map(|event| AnimationEvent::new(event.time, event.name.clone()))
            .collect();

        // Through the library, like the footstep: a packed export has
        // no `textures/` directory to read from.
        let skin_path = "textures/skin.png";
        let skin_bytes = library
            .read_path(skin_path)
            .map_err(|err| GameError::Setup(format!("{skin_path}: {err}")))?
            .ok_or_else(|| GameError::Setup(format!("{skin_path} is missing")))?;
        let skin_image = engine::asset::import_texture_bytes(&skin_bytes)
            .map_err(|err| GameError::Setup(format!("{skin_path}: {err}")))?;

        let parts = ctx.scene_parts();
        let skin_texture = parts.gpu.create_texture_from_rgba(
            "player skin",
            skin_image.width,
            skin_image.height,
            skin_image.mip_levels.first().map_or(&[][..], Vec::as_slice),
        );
        let skinned_mesh = parts
            .gpu
            .create_skinned_mesh("player", &character.vertices, &character.indices)
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

        self.body = Some(
            parts
                .world
                .spawn((
                    engine::ecs::components::Transform::from(Transform::from_translation(spawn)),
                    engine::ecs::components::GlobalTransform::default(),
                    renderer,
                    AnimationPlayer::new(character.skeleton.clone(), clip).with_events(footsteps),
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
        if let Some(sound) = footstep_sound(&library) {
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
        build_ui(self, ctx);

        tracing::info!("island ready — WASD to walk, drag to orbit, Escape to pause");
        Ok(())
    }

    fn fixed_update(&mut self, ctx: &mut GameContext<'_>, step: f32) {
        if self.paused {
            return;
        }
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

        // Escape / Start toggles the menu. Rebuilding the tree is what
        // shows and hides it: node ids are only stable until the next
        // `Ui::clear`, so the buttons are re-registered each time rather
        // than kept around as hidden zero-sized nodes.
        if self.actions.is_pressed(Action::Pause, ctx.input()) {
            self.paused = !self.paused;
            build_ui(self, ctx);
        }

        if self.paused {
            // Activation reaches here identically whether the player
            // clicked the button, pressed Enter, or pressed A — the
            // engine folds all three into one list.
            if self.menu_resume.is_some_and(|id| ctx.was_clicked(id)) {
                self.paused = false;
                build_ui(self, ctx);
            } else if self.menu_quit.is_some_and(|id| ctx.was_clicked(id)) {
                ctx.request_exit();
            }
            // Nothing else runs: no camera orbit, no walking, no
            // footsteps. `fixed_update` bails out too, so physics is
            // frozen rather than merely unobserved.
            return;
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

/// Rebuilds the whole UI tree: the HUD always, the pause menu when
/// [`Island::paused`].
///
/// One function rather than two because [`Ui::clear`] invalidates every
/// node id, so the HUD's ids have to be re-registered whenever the menu
/// appears or disappears.
fn build_ui(island: &mut Island, ctx: &mut GameContext<'_>) {
    build_hud(island, ctx);
    if island.paused {
        build_pause_menu(island, ctx);
    } else {
        island.menu_resume = None;
        island.menu_quit = None;
    }
    // Laid out here rather than waiting for the engine's own pass, so
    // directional focus works on the very frame the menu opens —
    // `focus_direction` scores nodes by their laid-out rectangles.
    ctx.ui_mut().layout();
}

/// Builds the pause menu: a dimming backdrop, a title, and two buttons
/// stacked so the d-pad's up/down maps to the order they are read in.
///
/// Focus starts on Resume, so a controller player can press A
/// immediately without first hunting for the cursor.
fn build_pause_menu(island: &mut Island, ctx: &mut GameContext<'_>) {
    let ui = ctx.ui_mut();

    let backdrop = ui.add(
        None,
        UiStyle {
            anchor: Anchor::Center,
            offset: [0.0, 0.0],
            size: [320.0, 200.0],
        },
        UiWidget::Panel {
            color: UiColor {
                r: 0.03,
                g: 0.04,
                b: 0.06,
                a: 0.85,
            },
        },
    );

    ui.add(
        Some(backdrop),
        UiStyle {
            anchor: Anchor::Top,
            offset: [0.0, 24.0],
            size: [200.0, 20.0],
        },
        UiWidget::Label {
            text: "PAUSED".to_string(),
            color: UiColor {
                r: 0.92,
                g: 0.95,
                b: 1.0,
                a: 1.0,
            },
        },
    );

    let mut button = |slot: &mut Option<NodeId>, y: f32, caption: &str| {
        let node = ui.add(
            Some(backdrop),
            UiStyle {
                anchor: Anchor::Center,
                offset: [0.0, y],
                size: [180.0, 36.0],
            },
            UiWidget::Button {
                text: caption.to_string(),
                bg: UiColor {
                    r: 0.16,
                    g: 0.20,
                    b: 0.26,
                    a: 1.0,
                },
                hot_bg: UiColor {
                    r: 0.26,
                    g: 0.34,
                    b: 0.44,
                    a: 1.0,
                },
            },
        );
        *slot = Some(node);
    };

    button(&mut island.menu_resume, -8.0, "Resume");
    button(&mut island.menu_quit, 40.0, "Quit");

    if let Some(resume) = island.menu_resume {
        ui.set_focus(Some(resume));
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
    label(&mut hint, 56.0, "WASD / stick to walk - Esc to pause");
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

/// Where this project lives.
///
/// First CLI argument, so a packaged build can point at the folder it
/// ships beside; otherwise the crate's own directory, which is what
/// makes a bare `cargo run -p island` work during development.
fn project_root() -> std::path::PathBuf {
    std::env::args().nth(1).map_or_else(
        || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        std::path::PathBuf::from,
    )
}

/// Loads the footfall from the project's own `audio/footstep.wav`.
///
/// Through the [`AssetLibrary`], not through a file path: in a shipped
/// export that file is inside `assets.pak` and there is no directory to
/// open. The file the baker wrote is still the only source — replacing
/// that `.wav` and re-baking changes the game without touching a line of
/// code.
fn footstep_sound(library: &AssetLibrary) -> Option<StaticSound> {
    let relative = "audio/footstep.wav";
    let bytes = match library.read_path(relative) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::warn!(path = relative, "no footstep sound in this project");
            return None;
        }
        Err(err) => {
            tracing::warn!(error = %err, path = relative, "could not read the footstep sound");
            return None;
        }
    };
    // One-shot: retriggered per footstep, never looped.
    match StaticSound::from_bytes(bytes, false) {
        Ok(sound) => Some(sound),
        Err(err) => {
            tracing::warn!(error = %err, path = relative, "footstep sound could not be decoded");
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
