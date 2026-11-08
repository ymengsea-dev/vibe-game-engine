//! The game's camera: which camera object is active, and the view it presents.
//!
//! A camera is an object. It looks along its object's −Z with up = +Y, so
//! moving or rotating the object (a script, an animation, a parent) moves the
//! view. [`GameplayRuntime::present_camera`] copies that view to the renderer
//! every frame.

use std::collections::HashSet;

use engine_ecs::AudioListener;
use engine_ecs::components::{
    ActiveCamera, Camera as CameraComponent, CameraFollow, FollowMode, GlobalTransform, OrbitInput,
    ThirdPersonState, Transform as TransformComponent,
};
use engine_ecs::prelude::{ChildOf, Entity, World};
use engine_scene::{SceneObjectId, camera_world_pose, look_rotation};
use engine_scripting::GameCommand;
use engine_utils::Rng;
use glam::{Mat3, Mat4, Quat, Vec2, Vec3};

use super::GameplayRuntime;
use super::commands::find_object;
use crate::app::GameContext;

/// The camera entity the game views through.
///
/// `scene_order` is the scene's entities in scene order. Among those that are
/// still cameras, the first with [`ActiveCamera`] wins, else the first camera.
/// When none qualify, other camera entities (spawned at runtime) are ordered
/// by entity index, again preferring [`ActiveCamera`].
pub fn choose_active_camera(world: &mut World, scene_order: &[Entity]) -> Option<Entity> {
    // A disabled camera object is switched off; never view through it.
    let is_camera = |world: &World, entity: Entity| {
        world.get::<CameraComponent>(entity).is_some()
            && world
                .get::<engine_ecs::components::Disabled>(entity)
                .is_none()
    };
    let is_active = |world: &World, entity: Entity| world.get::<ActiveCamera>(entity).is_some();
    let listed: Vec<Entity> = scene_order
        .iter()
        .copied()
        .filter(|entity| is_camera(world, *entity))
        .collect();
    let candidates = if listed.is_empty() {
        let mut others: Vec<Entity> = world
            .query::<(Entity, &CameraComponent)>()
            .iter(world)
            .map(|(entity, _)| entity)
            .filter(|entity| is_camera(world, *entity))
            .collect();
        others.sort_by_key(|entity| entity.index());
        others
    } else {
        listed
    };
    candidates
        .iter()
        .copied()
        .find(|entity| is_active(world, *entity))
        .or_else(|| candidates.first().copied())
}

/// The renderer camera for a camera object whose world matrix is `world`:
/// eye, target and up from the matrix, everything else from `camera`.
pub fn view_from_world(
    camera: &engine_renderer::Camera,
    world: Mat4,
    aspect: f32,
) -> engine_renderer::Camera {
    let (eye, target, up) = camera_world_pose(world);
    engine_renderer::Camera {
        eye,
        target,
        up,
        aspect_ratio: aspect,
        ..*camera
    }
}

/// The view `entity` presents, or `None` when it is not a camera. An object
/// with no transform keeps the camera component's own view.
fn presented_view(world: &World, entity: Entity, aspect: f32) -> Option<engine_renderer::Camera> {
    let camera = world.get::<CameraComponent>(entity)?.0;
    let matrix = world
        .get::<GlobalTransform>(entity)
        .map(|global| global.0.to_matrix())
        .or_else(|| {
            world
                .get::<TransformComponent>(entity)
                .map(|local| local.0.to_matrix())
        });
    Some(match matrix {
        Some(matrix) => view_from_world(&camera, matrix, aspect),
        None => engine_renderer::Camera {
            aspect_ratio: aspect,
            ..camera
        },
    })
}

/// Where the audio listener sits for `camera`: at the eye, facing the view.
pub(super) fn listener_transform(camera: &engine_renderer::Camera) -> engine_utils::Transform {
    let mut transform = engine_utils::Transform::from_translation(camera.eye);
    if camera.is_valid() {
        let pose = camera.view_matrix().inverse();
        transform.rotation = Quat::from_mat3(&Mat3::from_mat4(pose));
    }
    transform
}

/// Mouse orbit, radians per pixel of motion.
const MOUSE_ORBIT_RADIANS_PER_PIXEL: f32 = 0.003;
/// Right-stick orbit, radians per second at full tilt.
const STICK_ORBIT_RADIANS_PER_SECOND: f32 = 2.5;

/// This frame's input for follow cameras.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct FollowInput {
    /// Seconds since the last frame.
    pub dt: f32,
    /// Raw mouse motion this frame, in pixels.
    pub mouse: Vec2,
    /// Right thumbstick `(x, y)`, radially deadzoned; up is positive `y`.
    pub right_stick: Vec2,
}

/// Third-person follow settings, in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThirdPersonSettings {
    pub distance: f32,
    pub height: f32,
    pub smoothing: f32,
    pub pitch_min: f32,
    pub pitch_max: f32,
}

/// 2D side-follow settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Side2DSettings {
    pub dead_zone: Vec2,
    /// `min_x, min_y, max_x, max_y`.
    pub bounds: Option<[f32; 4]>,
    pub smoothing: f32,
}

/// How far to orbit this frame, as `(yaw-ish x, pitch y)` in radians: `x` is
/// positive for "look right", `y` positive for "look down". Mouse motion
/// already has that sense; the stick's up is positive, so `y` is flipped.
pub(crate) fn orbit_delta(orbit: OrbitInput, input: &FollowInput) -> Vec2 {
    let mouse = input.mouse * MOUSE_ORBIT_RADIANS_PER_PIXEL;
    let stick = Vec2::new(input.right_stick.x, -input.right_stick.y)
        * STICK_ORBIT_RADIANS_PER_SECOND
        * input.dt.max(0.0);
    let delta = match orbit {
        OrbitInput::None => Vec2::ZERO,
        OrbitInput::Mouse => mouse,
        OrbitInput::RightStick => stick,
        OrbitInput::Both => mouse + stick,
    };
    if delta.is_finite() { delta } else { Vec2::ZERO }
}

/// One step of third-person follow: orbits by `orbit_delta`, smooths the eye
/// toward the spot behind and above `target` (frame-rate independent) and
/// returns the camera's world eye and rotation (looking at the target).
pub(crate) fn follow_third_person(
    state: &mut ThirdPersonState,
    target: Vec3,
    orbit_delta: Vec2,
    dt: f32,
    settings: &ThirdPersonSettings,
) -> (Vec3, Quat) {
    let delta = if orbit_delta.is_finite() {
        orbit_delta
    } else {
        Vec2::ZERO
    };
    let rig = &mut state.rig;
    rig.target = target;
    rig.distance = settings.distance;
    rig.height = settings.height;
    rig.follow_speed = settings.smoothing;
    // Looking right swings the eye the other way round the target.
    rig.orbit(-delta.x, 0.0);
    // `max`/`min`, not `clamp`: odd limits must not panic.
    rig.pitch = (rig.pitch + delta.y)
        .max(settings.pitch_min)
        .min(settings.pitch_max);
    let start = state.eye.unwrap_or_else(|| rig.desired_eye());
    let mut view = engine_renderer::Camera::new(start, rig.look_point(), 1.0);
    if settings.smoothing > 0.0 {
        rig.apply(&mut view, dt);
    } else {
        // Smoothing 0 means none: the eye is always at its goal.
        view.eye = rig.desired_eye();
    }
    let eye = if view.eye.is_finite() {
        view.eye
    } else {
        rig.desired_eye()
    };
    state.eye = Some(eye);
    // A degenerate look keeps the last good rotation, never an identity flip.
    let rotation = look_rotation(rig.look_point() - eye, Vec3::Y).unwrap_or(state.rotation);
    state.rotation = rotation;
    (eye, rotation)
}

/// On the first follow frame, takes yaw and pitch from where the camera was
/// authored relative to the target's look point, so Play starts there. Pitch
/// is clamped to the settings; distance still comes from the settings.
pub(crate) fn seed_third_person(
    state: &mut ThirdPersonState,
    camera_eye: Vec3,
    target: Vec3,
    settings: &ThirdPersonSettings,
) {
    let look_point = target + Vec3::Y * settings.height;
    let offset = camera_eye - look_point;
    let length = offset.length();
    if !(length.is_finite() && length > 1e-4) {
        return;
    }
    state.rig.yaw = offset.x.atan2(offset.z);
    state.rig.pitch = (offset.y / length)
        .clamp(-1.0, 1.0)
        .asin()
        .max(settings.pitch_min)
        .min(settings.pitch_max);
    state.eye = Some(camera_eye);
}

/// On the first first-person frame, starts looking the way the camera was
/// authored. Pitch is clamped to the limits, given in radians.
pub(crate) fn seed_first_person(
    state: &mut ThirdPersonState,
    rotation: Quat,
    pitch_min: f32,
    pitch_max: f32,
) {
    let forward = rotation * Vec3::NEG_Z;
    if forward.is_finite() {
        state.rig.yaw = (-forward.x).atan2(-forward.z);
        state.rig.pitch = forward
            .y
            .clamp(-1.0, 1.0)
            .asin()
            .max(pitch_min)
            .min(pitch_max);
    }
}

/// One step of first-person follow: turns by `look_delta` (`x` positive looks
/// right, `y` positive looks down) and returns the eye, `height` above
/// `target`, and the camera's rotation.
pub(crate) fn follow_first_person(
    state: &mut ThirdPersonState,
    target: Vec3,
    look_delta: Vec2,
    height: f32,
    pitch_min: f32,
    pitch_max: f32,
) -> (Vec3, Quat) {
    let delta = if look_delta.is_finite() {
        look_delta
    } else {
        Vec2::ZERO
    };
    let rig = &mut state.rig;
    rig.yaw -= delta.x;
    // `max`/`min`, not `clamp`: odd limits must not panic.
    rig.pitch = (rig.pitch - delta.y).max(pitch_min).min(pitch_max);
    let rotation = Quat::from_rotation_y(rig.yaw) * Quat::from_rotation_x(rig.pitch);
    let eye = target + Vec3::Y * height;
    state.eye = Some(eye);
    state.rotation = rotation;
    (eye, rotation)
}

/// Whether the mouse should be locked for `camera`: it is a first- or
/// third-person camera the mouse turns, and the game is being played.
fn follow_wants_mouse(world: &World, camera: Option<Entity>) -> bool {
    let turned_by_mouse = camera
        .and_then(|camera| world.get::<CameraFollow>(camera))
        .is_some_and(|follow| {
            matches!(
                follow.mode,
                FollowMode::FirstPerson {
                    look: OrbitInput::Mouse | OrbitInput::Both,
                    ..
                } | FollowMode::ThirdPerson {
                    orbit: OrbitInput::Mouse | OrbitInput::Both,
                    ..
                }
            )
        });
    turned_by_mouse && super::state::current(world) == engine_scene::GameStateName::Playing
}

/// Swaps `camera`'s view with its [`ViewSwitch`](engine_ecs::components::ViewSwitch)
/// one, still looking the same way. Returns whether it switched.
fn switch_view(world: &mut World, camera: Entity) -> bool {
    let Some(mut follow) = world.get::<CameraFollow>(camera).cloned() else {
        return false;
    };
    let Some(switch) = follow.switch.as_mut() else {
        return false;
    };
    std::mem::swap(&mut follow.mode, &mut switch.other);
    // A first-person camera seeds itself from the current rotation.
    follow.state = ThirdPersonState::default();
    if let FollowMode::ThirdPerson {
        distance,
        height,
        smoothing,
        pitch_min_degrees,
        pitch_max_degrees,
        ..
    } = follow.mode
        && let Some(target) = follow_target_position(world, &follow.target, camera)
        && let Some(rotation) = world
            .get::<TransformComponent>(camera)
            .map(|local| local.0.rotation)
    {
        // Start already behind the target, on the line the camera looks along.
        let settings = ThirdPersonSettings {
            distance,
            height,
            smoothing,
            pitch_min: pitch_min_degrees.to_radians(),
            pitch_max: pitch_max_degrees.to_radians(),
        };
        let eye = target + Vec3::Y * height - rotation * Vec3::NEG_Z * distance;
        seed_third_person(&mut follow.state, eye, target, &settings);
    }
    if let Some(mut live) = world.get_mut::<CameraFollow>(camera) {
        *live = follow;
    }
    true
}

/// One step of 2D side follow. Moves x/y only once `target` leaves the dead
/// zone centred on `current`, smooths, then clamps to the bounds. z stays.
pub(crate) fn follow_side_2d(
    current: Vec3,
    target: Vec3,
    dt: f32,
    settings: &Side2DSettings,
) -> Vec3 {
    let outside = |delta: f32, zone: f32| delta.signum() * (delta.abs() - zone).max(0.0);
    let excess = Vec2::new(
        outside(target.x - current.x, settings.dead_zone.x),
        outside(target.y - current.y, settings.dead_zone.y),
    );
    let rate = settings.smoothing.max(0.0) * dt.max(0.0);
    let alpha = if settings.smoothing <= 0.0 {
        1.0
    } else if rate.is_finite() {
        (1.0 - (-rate).exp()).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let mut x = current.x + excess.x * alpha;
    let mut y = current.y + excess.y * alpha;
    if let Some([min_x, min_y, max_x, max_y]) = settings.bounds {
        x = x.max(min_x).min(max_x);
        y = y.max(min_y).min(max_y);
    }
    if x.is_finite() && y.is_finite() {
        Vec3::new(x, y, current.z)
    } else {
        current
    }
}

fn warn_once(
    warned: &mut HashSet<(String, &'static str)>,
    key: String,
    kind: &'static str,
    reason: &str,
) {
    if warned.insert((key.clone(), kind)) {
        tracing::warn!(camera = %key, kind, reason, "camera follow skipped");
    }
}

/// Where the object `target` names is, unless it is `camera` itself.
fn follow_target_position(world: &World, target: &str, camera: Entity) -> Option<Vec3> {
    find_object(world, target)
        .filter(|target| *target != camera)
        .and_then(|target| {
            world
                .get::<GlobalTransform>(target)
                .map(|global| global.0.translation)
                .or_else(|| {
                    world
                        .get::<TransformComponent>(target)
                        .map(|local| local.0.translation)
                })
        })
        .filter(|position| position.is_finite())
}

/// Moves the camera object `camera` toward its follow target. Returns whether
/// its `Transform` was written. A missing target, a parented camera or a
/// non-finite position leaves it alone and warns once.
fn apply_follow(
    world: &mut World,
    camera: Entity,
    input: &FollowInput,
    warned: &mut HashSet<(String, &'static str)>,
) -> bool {
    let Some(follow) = world.get::<CameraFollow>(camera).cloned() else {
        return false;
    };
    let name = world
        .get::<SceneObjectId>(camera)
        .map_or_else(|| format!("{camera:?}"), |id| id.0.clone());
    if world.get::<ChildOf>(camera).is_some() {
        warn_once(
            warned,
            name,
            "camera_follow_parented",
            "a follow camera cannot have a parent",
        );
        return false;
    }
    let Some(current) = world.get::<TransformComponent>(camera).map(|local| local.0) else {
        return false;
    };
    let Some(target_position) = follow_target_position(world, &follow.target, camera) else {
        warn_once(
            warned,
            format!("{name}->{}", follow.target),
            "camera_follow_target",
            "the follow target was not found",
        );
        return false;
    };

    let mut updated = current;
    let mut state = follow.state;
    match follow.mode {
        FollowMode::ThirdPerson {
            distance,
            height,
            smoothing,
            orbit,
            pitch_min_degrees,
            pitch_max_degrees,
        } => {
            let settings = ThirdPersonSettings {
                distance,
                height,
                smoothing,
                pitch_min: pitch_min_degrees.to_radians(),
                pitch_max: pitch_max_degrees.to_radians(),
            };
            if state.eye.is_none() {
                seed_third_person(&mut state, current.translation, target_position, &settings);
            }
            let (eye, rotation) = follow_third_person(
                &mut state,
                target_position,
                orbit_delta(orbit, input),
                input.dt,
                &settings,
            );
            updated.translation = eye;
            updated.rotation = rotation;
        }
        FollowMode::FirstPerson {
            height,
            look,
            pitch_min_degrees,
            pitch_max_degrees,
        } => {
            let (pitch_min, pitch_max) = (
                pitch_min_degrees.to_radians(),
                pitch_max_degrees.to_radians(),
            );
            if state.eye.is_none() {
                seed_first_person(&mut state, current.rotation, pitch_min, pitch_max);
            }
            let (eye, rotation) = follow_first_person(
                &mut state,
                target_position,
                orbit_delta(look, input),
                height,
                pitch_min,
                pitch_max,
            );
            updated.translation = eye;
            updated.rotation = rotation;
        }
        FollowMode::Side2D {
            dead_zone,
            bounds,
            smoothing,
        } => {
            let settings = Side2DSettings {
                dead_zone: Vec2::from_array(dead_zone),
                bounds,
                smoothing,
            };
            updated.translation =
                follow_side_2d(current.translation, target_position, input.dt, &settings);
        }
    }
    if let Some(mut live) = world.get_mut::<CameraFollow>(camera) {
        live.state = state;
    }
    if let Some(mut local) = world.get_mut::<TransformComponent>(camera) {
        local.0 = updated;
    }
    true
}

/// Seed for the shake's random source, fixed so a scene replays the same way.
pub(super) const SHAKE_SEED: u64 = 0x5eed_ca3e;

/// Longest shake, in seconds (the same limit as the Lua API).
const MAX_SHAKE_SECONDS: f32 = 10.0;

/// A camera shake in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shake {
    /// Largest offset, in world units, at the start.
    pub strength: f32,
    /// Seconds the shake lasts.
    pub total: f32,
    /// Seconds left.
    pub remaining: f32,
}

fn warn_command_once(
    warned: &mut HashSet<(String, &'static str)>,
    id: &str,
    kind: &'static str,
    reason: &str,
) {
    if warned.insert((id.to_owned(), kind)) {
        tracing::warn!(object = %id, kind, reason, "camera command skipped");
    }
}

/// Applies the camera commands of a batch, in order. Run after spawns so a
/// script can spawn a camera and switch to it in one callback. Other commands
/// are ignored.
pub(crate) fn apply_camera_commands(
    world: &mut World,
    active: &mut Option<Entity>,
    shake: &mut Option<Shake>,
    warned: &mut HashSet<(String, &'static str)>,
    commands: &[GameCommand],
) {
    for command in commands {
        match command {
            GameCommand::SetCamera { object_id } => {
                match find_object(world, object_id)
                    .filter(|entity| world.get::<CameraComponent>(*entity).is_some())
                {
                    Some(entity) => *active = Some(entity),
                    None => warn_command_once(
                        warned,
                        object_id,
                        "set_camera",
                        "object is not a camera; keeping the current camera",
                    ),
                }
            }
            GameCommand::SetFollowTarget {
                camera_id,
                target_id,
            } => {
                let follow = find_object(world, camera_id)
                    .and_then(|entity| world.get_mut::<CameraFollow>(entity));
                match follow {
                    Some(mut follow) => {
                        if follow.target != *target_id {
                            follow.target.clone_from(target_id);
                            // Re-seed from the current pose next frame: no
                            // snap, no stale yaw or pitch.
                            follow.state = ThirdPersonState::default();
                        }
                    }
                    None => warn_command_once(
                        warned,
                        camera_id,
                        "set_follow_target",
                        "object has no camera follow",
                    ),
                }
            }
            GameCommand::CameraShake { strength, seconds } => {
                // Same cap as the Lua API; a non-finite value cancels.
                let finite = seconds.is_finite();
                let seconds = seconds.min(MAX_SHAKE_SECONDS);
                *shake = (strength.is_finite() && *strength > 0.0 && finite && seconds > 0.0)
                    .then_some(Shake {
                        strength: *strength,
                        total: seconds,
                        remaining: seconds,
                    });
            }
            _ => {}
        }
    }
}

/// The scene id of the camera object the game views through.
pub(crate) fn active_camera_id(world: &World, active: Option<Entity>) -> Option<String> {
    let entity = active.filter(|entity| world.get::<CameraComponent>(*entity).is_some())?;
    world.get::<SceneObjectId>(entity).map(|id| id.0.clone())
}

/// Advances `shake` by `dt` and returns `view` moved by the shake's offset: a
/// random unit vector in the camera's right/up plane times
/// `strength * remaining / total`, applied to eye and target alike. The offset
/// is zero when the shake ends, and the shake is dropped then.
pub(crate) fn apply_shake(
    view: engine_renderer::Camera,
    shake: &mut Option<Shake>,
    rng: &mut Rng,
    dt: f32,
) -> engine_renderer::Camera {
    let Some(active) = shake.as_mut() else {
        return view;
    };
    active.remaining = (active.remaining - dt.max(0.0)).max(0.0);
    let magnitude = active.strength * active.remaining / active.total;
    if active.remaining <= 0.0 {
        *shake = None;
    }
    let forward = (view.target - view.eye).try_normalize();
    let right = forward.and_then(|forward| forward.cross(view.up).try_normalize());
    let (Some(forward), Some(right)) = (forward, right) else {
        return view;
    };
    let up = right.cross(forward);
    let angle = rng.range(0.0, std::f32::consts::TAU);
    let offset = (right * angle.cos() + up * angle.sin()) * magnitude;
    if !offset.is_finite() {
        return view;
    }
    engine_renderer::Camera {
        eye: view.eye + offset,
        target: view.target + offset,
        ..view
    }
}

impl GameplayRuntime {
    /// The camera object the game currently views through.
    pub fn active_camera(&self) -> Option<Entity> {
        self.active_camera
    }

    /// The scene id of the camera object the game views through, if it has one.
    pub fn active_camera_id(&self, world: &World) -> Option<String> {
        active_camera_id(world, self.active_camera)
    }

    /// Presents the active camera object's view for this frame: propagates
    /// transforms, copies the view to the renderer (keeping the live window
    /// aspect) and moves the audio listener with it. Call at the end of
    /// `update`, after scripts and scene changes. With no camera object the
    /// current view is left alone.
    pub fn present_camera(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        let dt = super::state::camera_dt(ctx.simulation_paused(), dt);
        let aspect = ctx.camera_mut().aspect_ratio;
        // Orbit reads raw mouse motion, which only arrives while the game has
        // the pointer (see the Scene View Play focus gate). A zero-length step
        // (scene setup) never orbits.
        let input = FollowInput {
            dt,
            mouse: if dt > 0.0 {
                let (x, y) = ctx.input().mouse_delta();
                Vec2::new(x, y)
            } else {
                Vec2::ZERO
            },
            right_stick: {
                let (x, y) = ctx.input().stick(engine_platform::Stick::Right);
                Vec2::new(x, y)
            },
        };
        if !ctx.simulation_paused()
            && let Some(camera) = self.active_camera
        {
            let key = ctx
                .world()
                .get::<CameraFollow>(camera)
                .and_then(|follow| follow.switch.as_ref())
                .and_then(|switch| engine_scripting::ScriptKey::from_name(&switch.key));
            if key.is_some_and(|key| ctx.input().is_key_pressed(super::keys::platform_key(key))) {
                switch_view(ctx.world_mut(), camera);
            }
        }
        let view = present_in_world(
            ctx.world_mut(),
            &mut self.active_camera,
            &self.scene_order,
            aspect,
            &input,
            &mut self.warned,
        );
        // Only on a change, so a script can still free or lock the mouse itself.
        let wants_mouse = follow_wants_mouse(ctx.world_mut(), self.active_camera);
        if wants_mouse != self.follow_mouse {
            self.follow_mouse = wants_mouse;
            ctx.set_mouse_locked(wants_mouse);
        }
        let Some(view) = view else {
            return;
        };
        // The listener stays on the steady view; only the picture shakes.
        let transform = listener_transform(&view);
        *ctx.camera_mut() = apply_shake(view, &mut self.shake, &mut self.shake_rng, dt);

        let world = ctx.world_mut();
        let listener = world
            .query_filtered::<Entity, engine_ecs::prelude::With<AudioListener>>()
            .iter(world)
            .next();
        if let Some(listener) = listener {
            world.entity_mut(listener).insert((
                TransformComponent::from(transform),
                GlobalTransform::from(transform),
            ));
        }
    }
}

/// The GPU-free half of [`GameplayRuntime::present_camera`]: propagates
/// transforms, re-chooses the camera when `current` is unset or no longer a
/// camera, moves it if it follows an object, and returns its view at `aspect`
/// (`None` when there is no camera).
pub(crate) fn present_in_world(
    world: &mut World,
    current: &mut Option<Entity>,
    scene_order: &[Entity],
    aspect: f32,
    input: &FollowInput,
    warned: &mut HashSet<(String, &'static str)>,
) -> Option<engine_renderer::Camera> {
    engine_ecs::propagate_transforms(world);
    // A camera object that was disabled or destroyed is replaced.
    let valid = current.is_some_and(|entity| {
        world.get::<CameraComponent>(entity).is_some()
            && world
                .get::<engine_ecs::components::Disabled>(entity)
                .is_none()
    });
    if !valid {
        *current = choose_active_camera(world, scene_order);
    }
    let camera = (*current)?;
    if apply_follow(world, camera, input, warned) {
        engine_ecs::propagate_transforms(world);
    }
    presented_view(world, camera, aspect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;
    use engine_ecs::components::{CameraFollow, OrbitInput};
    use engine_ecs::components::{GlobalTransform, Transform};
    use engine_ecs::prelude::ChildOf;
    use engine_ecs::propagate_transforms;
    use engine_renderer::CameraRig;
    use engine_scene::SceneObjectId;
    use engine_scripting::GameCommand;
    use glam::{Quat, Vec2, Vec3};
    use std::collections::HashSet;

    fn cam() -> engine_renderer::Camera {
        engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.5)
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    fn spawn_cam(world: &mut World, active: bool, x: f32) -> Entity {
        let mut entity = world.spawn((
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                x, 0.0, 0.0,
            ))),
            CameraComponent(cam()),
        ));
        if active {
            entity.insert(ActiveCamera);
        }
        entity.id()
    }

    #[test]
    fn first_active_camera_wins() {
        let mut world = World::new();
        let a = spawn_cam(&mut world, false, 0.0);
        let b = spawn_cam(&mut world, true, 0.0);
        let c = spawn_cam(&mut world, true, 0.0);
        assert_eq!(choose_active_camera(&mut world, &[a, b, c]), Some(b));
    }

    #[test]
    fn falls_back_to_first_camera() {
        let mut world = World::new();
        let a = spawn_cam(&mut world, false, 0.0);
        let b = spawn_cam(&mut world, false, 0.0);
        assert_eq!(choose_active_camera(&mut world, &[a, b]), Some(a));
        assert_eq!(choose_active_camera(&mut World::new(), &[]), None);
    }

    #[test]
    fn scene_order_beats_spawn_order() {
        let mut world = World::new();
        let b = spawn_cam(&mut world, false, 0.0);
        let a = spawn_cam(&mut world, false, 0.0);
        assert_eq!(choose_active_camera(&mut world, &[a, b]), Some(a));
    }

    #[test]
    fn legacy_camera_without_object_id_still_counts() {
        // Neither entity has a SceneObjectId; only the order list matters.
        let mut world = World::new();
        let a = spawn_cam(&mut world, true, 0.0);
        let b = spawn_cam(&mut world, false, 0.0);
        assert_eq!(choose_active_camera(&mut world, &[a, b]), Some(a));
    }

    #[test]
    fn runtime_spawned_active_camera_only_counts_without_scene_cameras() {
        let mut world = World::new();
        let scene_cam = spawn_cam(&mut world, false, 0.0);
        let runtime_cam = spawn_cam(&mut world, true, 0.0);
        assert_eq!(
            choose_active_camera(&mut world, &[scene_cam]),
            Some(scene_cam)
        );
        world.despawn(scene_cam);
        assert_eq!(
            choose_active_camera(&mut world, &[scene_cam]),
            Some(runtime_cam)
        );
    }

    #[test]
    fn present_re_chooses_after_the_active_camera_is_despawned() {
        let mut world = World::new();
        let a = spawn_cam(&mut world, false, 1.0);
        let b = spawn_cam(&mut world, false, 2.0);
        let mut current = None;
        let view = present(&mut world, &mut current, &[a, b], 1.0).unwrap();
        assert_eq!(current, Some(a));
        assert!(near(view.eye, Vec3::new(1.0, 0.0, 0.0)));
        world.despawn(a);
        let view = present(&mut world, &mut current, &[a, b], 1.0).unwrap();
        assert_eq!(current, Some(b));
        assert!(near(view.eye, Vec3::new(2.0, 0.0, 0.0)));
    }

    #[test]
    fn present_without_cameras_returns_none() {
        let mut world = World::new();
        let mut current = None;
        assert!(present(&mut world, &mut current, &[], 1.0).is_none());
        assert_eq!(current, None);
    }

    #[test]
    fn present_uses_the_passed_aspect() {
        let mut world = World::new();
        let a = spawn_cam(&mut world, false, 0.0);
        let mut current = None;
        let view = present(&mut world, &mut current, &[a], 2.5).unwrap();
        assert_eq!(view.aspect_ratio, 2.5);
    }

    #[test]
    fn view_follows_the_camera_object() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    0.0, 1.0, 5.0,
                ))),
                CameraComponent(cam()),
            ))
            .id();
        propagate_transforms(&mut world);
        let matrix = world.get::<GlobalTransform>(entity).unwrap().0.to_matrix();
        let view = view_from_world(&cam(), matrix, 2.0);
        assert!(near(view.eye, Vec3::new(0.0, 1.0, 5.0)));
        assert!(near(view.target, Vec3::new(0.0, 1.0, 4.0)));
        assert_eq!(view.aspect_ratio, 2.0);

        world.get_mut::<Transform>(entity).unwrap().0.rotation =
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        propagate_transforms(&mut world);
        let matrix = world.get::<GlobalTransform>(entity).unwrap().0.to_matrix();
        let view = view_from_world(&cam(), matrix, 2.0);
        assert!(near(view.target, Vec3::new(-1.0, 1.0, 5.0)));
    }

    fn present(
        world: &mut World,
        current: &mut Option<Entity>,
        order: &[Entity],
        aspect: f32,
    ) -> Option<engine_renderer::Camera> {
        present_in_world(
            world,
            current,
            order,
            aspect,
            &FollowInput::default(),
            &mut HashSet::new(),
        )
    }

    fn third() -> ThirdPersonSettings {
        ThirdPersonSettings {
            distance: 6.0,
            height: 1.5,
            smoothing: 10.0,
            pitch_min: (-60.0_f32).to_radians(),
            pitch_max: 70.0_f32.to_radians(),
        }
    }

    fn rig_state() -> ThirdPersonState {
        ThirdPersonState::default()
    }

    #[test]
    fn third_person_settles_behind_and_above_target() {
        let settings = third();
        let mut state = rig_state();
        state.rig.pitch = 0.0;
        state.eye = Some(Vec3::new(20.0, 5.0, -3.0));
        let (mut eye, mut rotation) = (Vec3::ZERO, Quat::IDENTITY);
        for _ in 0..600 {
            (eye, rotation) =
                follow_third_person(&mut state, Vec3::ZERO, Vec2::ZERO, 1.0 / 60.0, &settings);
        }
        let mut rig = CameraRig::new(Vec3::ZERO);
        rig.distance = 6.0;
        rig.height = 1.5;
        rig.pitch = 0.0;
        assert!(near(eye, rig.desired_eye()), "eye {eye:?}");
        assert!(
            eye.z > 0.0 && (eye.y - 1.5).abs() < 1e-3,
            "behind the target"
        );
        let forward = rotation * Vec3::NEG_Z;
        let want = (rig.look_point() - eye).normalize();
        assert!((forward - want).length() < 1e-3);
    }

    #[test]
    fn third_person_smoothing_is_frame_rate_independent() {
        let settings = third();
        let run = |steps: u32| {
            let mut state = rig_state();
            state.eye = Some(Vec3::new(10.0, 0.0, 0.0));
            let mut eye = Vec3::ZERO;
            for _ in 0..steps {
                eye = follow_third_person(
                    &mut state,
                    Vec3::ZERO,
                    Vec2::ZERO,
                    1.0 / steps as f32,
                    &settings,
                )
                .0;
            }
            eye
        };
        let (a, b) = (run(60), run(120));
        assert!((a - b).length() < 1e-3, "{a:?} vs {b:?}");
    }

    #[test]
    fn third_person_pitch_is_clamped() {
        let settings = third();
        let mut state = rig_state();
        follow_third_person(
            &mut state,
            Vec3::ZERO,
            Vec2::new(0.0, 1.0e6),
            0.016,
            &settings,
        );
        assert!((state.rig.pitch - settings.pitch_max).abs() < 1e-6);
        follow_third_person(
            &mut state,
            Vec3::ZERO,
            Vec2::new(0.0, -1.0e6),
            0.016,
            &settings,
        );
        assert!((state.rig.pitch - settings.pitch_min).abs() < 1e-6);
    }

    #[test]
    fn third_person_ignores_non_finite_orbit() {
        let settings = third();
        let mut state = rig_state();
        let (eye, rotation) = follow_third_person(
            &mut state,
            Vec3::ZERO,
            Vec2::new(f32::NAN, f32::INFINITY),
            0.016,
            &settings,
        );
        assert!(eye.is_finite() && rotation.is_finite());
    }

    #[test]
    fn first_person_sits_at_the_head_and_looks_where_authored() {
        let mut world = World::new();
        let target = world
            .spawn((
                SceneObjectId("player".into()),
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    5.0, 0.0, 0.0,
                ))),
            ))
            .id();
        let authored = Quat::from_rotation_y(0.5) * Quat::from_rotation_x(-0.25);
        let mut pose = engine_utils::Transform::from_translation(Vec3::new(9.0, 9.0, 9.0));
        pose.rotation = authored;
        let camera = world
            .spawn((
                Transform::from(pose),
                CameraComponent(cam()),
                CameraFollow::new("player", FollowMode::FIRST_PERSON),
            ))
            .id();
        propagate_transforms(&mut world);
        let mut warned = HashSet::new();
        let still = FollowInput {
            dt: 0.1,
            ..Default::default()
        };

        assert!(apply_follow(&mut world, camera, &still, &mut warned));
        let local = world.get::<Transform>(camera).unwrap().0;
        assert!(near(local.translation, Vec3::new(5.0, 1.6, 0.0)));
        assert!(near(local.rotation * Vec3::NEG_Z, authored * Vec3::NEG_Z));

        // It stays with the target, with no smoothing.
        world.get_mut::<Transform>(target).unwrap().0.translation = Vec3::new(7.0, 1.0, 0.0);
        propagate_transforms(&mut world);
        assert!(apply_follow(&mut world, camera, &still, &mut warned));
        let local = world.get::<Transform>(camera).unwrap().0;
        assert!(near(local.translation, Vec3::new(7.0, 2.6, 0.0)));
    }

    #[test]
    fn first_person_turns_the_way_the_mouse_moves_within_the_limits() {
        // Level, looking down -Z; a new rig starts at its own orbit angle.
        let level = || {
            let mut state = ThirdPersonState::default();
            seed_first_person(&mut state, Quat::IDENTITY, -1.0, 1.0);
            state
        };
        let mut state = level();
        let (limit_low, limit_high) = (-1.0, 1.0);
        let turn = |state: &mut ThirdPersonState, delta: Vec2| {
            follow_first_person(state, Vec3::ZERO, delta, 1.6, limit_low, limit_high).1
                * Vec3::NEG_Z
        };
        assert!(near(turn(&mut state, Vec2::ZERO), Vec3::NEG_Z));
        assert!(turn(&mut state, Vec2::new(0.5, 0.0)).x > 0.0, "looks right");
        let mut state = level();
        assert!(turn(&mut state, Vec2::new(0.0, 0.5)).y < 0.0, "looks down");
        let bottom = turn(&mut state, Vec2::new(0.0, 100.0));
        assert!(
            (bottom.y - f32::sin(limit_low)).abs() < 1e-5,
            "stops at the limit"
        );
        // Bad input leaves the view alone.
        let same = turn(&mut state, Vec2::new(f32::NAN, 1.0));
        assert!(near(same, bottom));
    }

    #[test]
    fn the_mouse_is_wanted_only_for_a_mouse_turned_camera_while_playing() {
        let mut world = World::new();
        let camera = spawn_cam(&mut world, true, 0.0);
        assert!(!follow_wants_mouse(&world, Some(camera)), "no follow");
        world
            .entity_mut(camera)
            .insert(CameraFollow::new("player", FollowMode::FIRST_PERSON));
        assert!(follow_wants_mouse(&world, Some(camera)));
        assert!(!follow_wants_mouse(&world, None));

        super::super::state::init_state(&mut world, None);
        world.resource_mut::<engine_scene::GameState>().current =
            engine_scene::GameStateName::Paused;
        assert!(!follow_wants_mouse(&world, Some(camera)), "paused");

        world.resource_mut::<engine_scene::GameState>().current =
            engine_scene::GameStateName::Playing;
        world.entity_mut(camera).insert(CameraFollow::new(
            "player",
            FollowMode::FirstPerson {
                height: 1.6,
                look: OrbitInput::RightStick,
                pitch_min_degrees: -85.0,
                pitch_max_degrees: 85.0,
            },
        ));
        assert!(!follow_wants_mouse(&world, Some(camera)), "stick only");
    }

    #[test]
    fn a_third_person_mouse_camera_wants_the_mouse_too() {
        let mut world = World::new();
        let camera = spawn_cam(&mut world, true, 0.0);
        world
            .entity_mut(camera)
            .insert(CameraFollow::new("player", FollowMode::THIRD_PERSON));
        assert!(follow_wants_mouse(&world, Some(camera)));
        world
            .entity_mut(camera)
            .insert(CameraFollow::new("player", FollowMode::SIDE_2D));
        assert!(!follow_wants_mouse(&world, Some(camera)));
    }

    #[test]
    fn switching_view_keeps_the_look_direction_and_both_settings() {
        let mut world = World::new();
        world.spawn((
            SceneObjectId("player".into()),
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                5.0, 0.0, 0.0,
            ))),
        ));
        let mut pose = engine_utils::Transform::IDENTITY;
        pose.rotation = Quat::from_rotation_y(0.5) * Quat::from_rotation_x(-0.25);
        let looking = pose.rotation * Vec3::NEG_Z;
        let first = FollowMode::FirstPerson {
            height: 1.7,
            look: OrbitInput::Mouse,
            pitch_min_degrees: -85.0,
            pitch_max_degrees: 85.0,
        };
        let third = FollowMode::ThirdPerson {
            distance: 4.0,
            height: 1.5,
            smoothing: 0.0,
            orbit: OrbitInput::Mouse,
            pitch_min_degrees: -60.0,
            pitch_max_degrees: 70.0,
        };
        let mut follow = CameraFollow::new("player", first);
        follow.switch = Some(engine_ecs::components::ViewSwitch {
            key: "V".into(),
            other: third,
        });
        let camera = world
            .spawn((Transform::from(pose), CameraComponent(cam()), follow))
            .id();
        propagate_transforms(&mut world);
        let mut warned = HashSet::new();
        let still = FollowInput {
            dt: 0.1,
            ..Default::default()
        };
        let forward =
            |world: &World| world.get::<Transform>(camera).unwrap().0.rotation * Vec3::NEG_Z;
        assert!(apply_follow(&mut world, camera, &still, &mut warned));

        // To third person: behind the head, looking the same way.
        assert!(switch_view(&mut world, camera));
        assert_eq!(world.get::<CameraFollow>(camera).unwrap().mode, third);
        assert!(apply_follow(&mut world, camera, &still, &mut warned));
        assert!(near(forward(&world), looking), "{}", forward(&world));
        let eye = world.get::<Transform>(camera).unwrap().0.translation;
        assert!(near(eye, Vec3::new(5.0, 1.5, 0.0) - looking * 4.0), "{eye}");

        // And back, with the first-person settings kept.
        assert!(switch_view(&mut world, camera));
        assert_eq!(world.get::<CameraFollow>(camera).unwrap().mode, first);
        assert!(apply_follow(&mut world, camera, &still, &mut warned));
        assert!(near(forward(&world), looking));
        let eye = world.get::<Transform>(camera).unwrap().0.translation;
        assert!(near(eye, Vec3::new(5.0, 1.7, 0.0)));
    }

    #[test]
    fn a_camera_with_no_switch_key_stays_as_it_is() {
        let mut world = World::new();
        let camera = spawn_cam(&mut world, true, 0.0);
        world
            .entity_mut(camera)
            .insert(CameraFollow::new("player", FollowMode::FIRST_PERSON));
        assert!(!switch_view(&mut world, camera));
    }

    #[test]
    fn orbit_input_none_ignores_mouse() {
        let input = FollowInput {
            dt: 0.5,
            mouse: Vec2::new(100.0, 50.0),
            right_stick: Vec2::new(1.0, 1.0),
        };
        assert_eq!(orbit_delta(OrbitInput::None, &input), Vec2::ZERO);
        let mouse = orbit_delta(OrbitInput::Mouse, &input);
        assert!((mouse - Vec2::new(0.3, 0.15)).length() < 1e-6);
        let stick = orbit_delta(OrbitInput::RightStick, &input);
        assert!((stick - Vec2::new(1.25, -1.25)).length() < 1e-6);
        let both = orbit_delta(OrbitInput::Both, &input);
        assert!((both - (mouse + stick)).length() < 1e-6);
    }

    #[test]
    fn side_2d_holds_inside_dead_zone_and_follows_outside() {
        let settings = Side2DSettings {
            dead_zone: Vec2::new(2.0, 1.0),
            bounds: None,
            smoothing: 8.0,
        };
        let current = Vec3::new(1.0, 2.0, 10.0);
        let held = follow_side_2d(current, Vec3::new(2.5, 2.5, 0.0), 0.1, &settings);
        assert_eq!(held, current);
        let mut position = current;
        for _ in 0..600 {
            position = follow_side_2d(position, Vec3::new(6.0, 2.0, 0.0), 1.0 / 60.0, &settings);
        }
        assert!(position.x > 1.5, "moved right");
        assert!(
            (position.x - 4.0).abs() < 1e-2,
            "settles at the dead-zone edge"
        );
        assert_eq!((position.y, position.z), (2.0, 10.0));
    }

    #[test]
    fn side_2d_respects_bounds() {
        let settings = Side2DSettings {
            dead_zone: Vec2::ZERO,
            bounds: Some([-1.0, -2.0, 3.0, 4.0]),
            smoothing: 100.0,
        };
        let mut position = Vec3::new(0.0, 0.0, 10.0);
        for _ in 0..120 {
            position = follow_side_2d(position, Vec3::new(50.0, 50.0, 0.0), 0.1, &settings);
        }
        assert!(near(position, Vec3::new(3.0, 4.0, 10.0)));
        for _ in 0..120 {
            position = follow_side_2d(position, Vec3::new(-50.0, -50.0, 0.0), 0.1, &settings);
        }
        assert!(near(position, Vec3::new(-1.0, -2.0, 10.0)));
    }

    fn spawn_target(world: &mut World, id: &str, at: Vec3) -> Entity {
        world
            .spawn((
                Transform::from(engine_utils::Transform::from_translation(at)),
                SceneObjectId(id.into()),
            ))
            .id()
    }

    fn spawn_follower(world: &mut World, follow: CameraFollow, at: Vec3) -> Entity {
        world
            .spawn((
                Transform::from(engine_utils::Transform::from_translation(at)),
                CameraComponent(cam()),
                SceneObjectId("camera-id".into()),
                follow,
            ))
            .id()
    }

    #[test]
    fn follow_with_missing_target_holds_position() {
        let mut world = World::new();
        let start = Vec3::new(3.0, 4.0, 5.0);
        let camera = spawn_follower(
            &mut world,
            CameraFollow::new("gone", engine_ecs::components::FollowMode::THIRD_PERSON),
            start,
        );
        let mut current = None;
        let mut warned = HashSet::new();
        let input = FollowInput {
            dt: 0.016,
            mouse: Vec2::new(5.0, 5.0),
            right_stick: Vec2::ZERO,
        };
        for _ in 0..3 {
            let view = present_in_world(
                &mut world,
                &mut current,
                &[camera],
                1.0,
                &input,
                &mut warned,
            )
            .unwrap();
            assert!(near(view.eye, start) && view.eye.is_finite());
            assert!(view.target.is_finite());
        }
        let moved = world.get::<Transform>(camera).unwrap().0.translation;
        assert!(near(moved, start));
        assert_eq!(warned.len(), 1, "one warning, not one per frame");

        // The target appears later: the camera now follows.
        spawn_target(&mut world, "gone", Vec3::new(0.0, 0.0, -20.0));
        let view = present_in_world(
            &mut world,
            &mut current,
            &[camera],
            1.0,
            &input,
            &mut warned,
        )
        .unwrap();
        assert!(
            view.eye.is_finite() && !near(view.eye, start),
            "now follows"
        );
    }

    #[test]
    fn follow_destroyed_target_holds_last_position() {
        let mut world = World::new();
        let target = spawn_target(&mut world, "t", Vec3::new(0.0, 0.0, -10.0));
        let camera = spawn_follower(
            &mut world,
            CameraFollow::new("t", engine_ecs::components::FollowMode::THIRD_PERSON),
            Vec3::ZERO,
        );
        let (mut current, mut warned) = (None, HashSet::new());
        let input = FollowInput::default();
        let first = present_in_world(
            &mut world,
            &mut current,
            &[camera],
            1.0,
            &input,
            &mut warned,
        )
        .unwrap();
        world.despawn(target);
        let second = present_in_world(
            &mut world,
            &mut current,
            &[camera],
            1.0,
            &input,
            &mut warned,
        )
        .unwrap();
        assert!(near(first.eye, second.eye));
        assert_eq!(warned.len(), 1);
    }

    #[test]
    fn follow_drives_the_camera_transform_for_side_2d() {
        let mut world = World::new();
        spawn_target(&mut world, "p", Vec3::new(10.0, 0.0, 0.0));
        let camera = spawn_follower(
            &mut world,
            CameraFollow::new("p", engine_ecs::components::FollowMode::SIDE_2D),
            Vec3::new(0.0, 0.0, 8.0),
        );
        let (mut current, mut warned) = (None, HashSet::new());
        let input = FollowInput {
            dt: 0.5,
            ..FollowInput::default()
        };
        let view = present_in_world(
            &mut world,
            &mut current,
            &[camera],
            1.0,
            &input,
            &mut warned,
        )
        .unwrap();
        let moved = world.get::<Transform>(camera).unwrap().0.translation;
        assert!(moved.x > 0.0 && moved.z == 8.0);
        assert!(near(view.eye, moved), "view uses the updated transform");
    }

    #[test]
    fn parented_camera_skips_follow_and_warns_once() {
        let mut world = World::new();
        spawn_target(&mut world, "p", Vec3::new(10.0, 0.0, 0.0));
        let parent = world
            .spawn(Transform::from(engine_utils::Transform::IDENTITY))
            .id();
        let camera = spawn_follower(
            &mut world,
            CameraFollow::new("p", engine_ecs::components::FollowMode::SIDE_2D),
            Vec3::ZERO,
        );
        world.entity_mut(camera).insert(ChildOf(parent));
        let (mut current, mut warned) = (None, HashSet::new());
        let input = FollowInput {
            dt: 0.5,
            ..FollowInput::default()
        };
        for _ in 0..3 {
            present_in_world(
                &mut world,
                &mut current,
                &[camera],
                1.0,
                &input,
                &mut warned,
            );
        }
        let held = world.get::<Transform>(camera).unwrap().0.translation;
        assert!(near(held, Vec3::ZERO));
        assert_eq!(warned.len(), 1);
    }

    #[test]
    fn smoothing_zero_snaps_to_the_goal() {
        let mut settings = third();
        settings.smoothing = 0.0;
        let mut state = rig_state();
        state.eye = Some(Vec3::new(30.0, 9.0, 4.0));
        let (eye, _) =
            follow_third_person(&mut state, Vec3::ZERO, Vec2::ZERO, 1.0 / 60.0, &settings);
        assert!(near(eye, state.rig.desired_eye()));
        let side = Side2DSettings {
            dead_zone: Vec2::new(2.0, 1.0),
            bounds: Some([-100.0, -100.0, 4.5, 100.0]),
            smoothing: 0.0,
        };
        let moved = follow_side_2d(
            Vec3::new(0.0, 0.0, 7.0),
            Vec3::new(9.0, 3.0, 0.0),
            1.0 / 60.0,
            &side,
        );
        assert!(near(moved, Vec3::new(4.5, 2.0, 7.0)), "{moved:?}");
    }

    #[test]
    fn first_follow_frame_keeps_the_authored_direction() {
        let settings = third();
        let target = Vec3::new(2.0, 0.0, -1.0);
        let authored = Vec3::new(-3.0, 4.0, 5.0) + target + Vec3::Y * settings.height;
        let mut world = World::new();
        spawn_target(&mut world, "t", target);
        let camera = spawn_follower(
            &mut world,
            CameraFollow::new("t", engine_ecs::components::FollowMode::THIRD_PERSON),
            authored,
        );
        let (mut current, mut warned) = (None, HashSet::new());
        let view = present_in_world(
            &mut world,
            &mut current,
            &[camera],
            1.0,
            &FollowInput {
                dt: 1.0e-6,
                ..FollowInput::default()
            },
            &mut warned,
        )
        .unwrap();
        let want = (authored - (target + Vec3::Y * settings.height)).normalize();
        let got = (view.eye - (target + Vec3::Y * settings.height)).normalize();
        assert!((want - got).length() < 1e-3, "{want:?} vs {got:?}");
    }

    #[test]
    fn old_scene_presents_the_same_view_as_before() {
        let ron = "(version: 5, entities: [(camera: Some((eye: (0.0, 2.0, 6.0), \
                   target: (0.0, 0.0, 0.0), up: (0.0, 1.0, 0.0), aspect_ratio: 1.5, \
                   projection: Perspective(fov_y_radians: 1.0), near: 0.1, far: 50.0)))])";
        let scene = Scene::from_ron_str(ron).unwrap();
        let mut world = World::new();
        scene.instantiate(&mut world);
        propagate_transforms(&mut world);
        let entity = choose_active_camera(&mut world, &[]).expect("camera");
        let component = world.get::<CameraComponent>(entity).unwrap().0;
        let matrix = world.get::<GlobalTransform>(entity).unwrap().0.to_matrix();
        let view = view_from_world(&component, matrix, 1.5);
        assert!(near(view.eye, Vec3::new(0.0, 2.0, 6.0)));
        assert!(near(
            (view.target - view.eye).normalize(),
            Vec3::new(0.0, -2.0, -6.0).normalize()
        ));
    }

    // ---- Task 5: script control ----

    fn spawn_object(world: &mut World, id: &str, camera: bool, x: f32) -> Entity {
        let mut entity = world.spawn((
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                x, 0.0, 0.0,
            ))),
            SceneObjectId(id.into()),
        ));
        if camera {
            entity.insert(CameraComponent(cam()));
        }
        entity.id()
    }

    fn set_camera(id: &str) -> GameCommand {
        GameCommand::SetCamera {
            object_id: id.into(),
        }
    }

    #[test]
    fn set_camera_switches_the_presented_view() {
        let mut world = World::new();
        let a = spawn_object(&mut world, "a", true, 0.0);
        let b = spawn_object(&mut world, "b", true, 7.0);
        let order = [a, b];
        let mut current = None;
        let first = present(&mut world, &mut current, &order, 1.0).unwrap();
        assert!(near(first.eye, Vec3::ZERO));
        let (mut shake, mut warned) = (None, HashSet::new());
        apply_camera_commands(
            &mut world,
            &mut current,
            &mut shake,
            &mut warned,
            &[set_camera("b")],
        );
        let second = present(&mut world, &mut current, &order, 1.0).unwrap();
        assert!(near(second.eye, Vec3::new(7.0, 0.0, 0.0)));
        assert_eq!(active_camera_id(&world, current).as_deref(), Some("b"),);
    }

    #[test]
    fn set_camera_to_non_camera_warns_once_and_keeps_view() {
        let mut world = World::new();
        let a = spawn_object(&mut world, "a", true, 0.0);
        spawn_object(&mut world, "rock", false, 3.0);
        let mut current = Some(a);
        let (mut shake, mut warned) = (None, HashSet::new());
        let batch = [set_camera("rock"), set_camera("rock"), set_camera("nope")];
        apply_camera_commands(&mut world, &mut current, &mut shake, &mut warned, &batch);
        assert_eq!(current, Some(a));
        let expected: HashSet<(String, &'static str)> = [
            ("rock".to_owned(), "set_camera"),
            ("nope".to_owned(), "set_camera"),
        ]
        .into_iter()
        .collect();
        assert_eq!(warned, expected);
    }

    #[test]
    fn set_follow_target_warns_without_follow_and_resets_state() {
        let mut world = World::new();
        spawn_object(&mut world, "plain", true, 0.0);
        spawn_target(&mut world, "old", Vec3::ZERO);
        spawn_target(&mut world, "new", Vec3::new(5.0, 0.0, 0.0));
        let rig = spawn_follower(
            &mut world,
            CameraFollow::new("old", engine_ecs::components::FollowMode::THIRD_PERSON),
            Vec3::new(0.0, 2.0, 6.0),
        );
        {
            let mut follow = world.get_mut::<CameraFollow>(rig).unwrap();
            follow.state.eye = Some(Vec3::splat(9.0));
            follow.state.rig.yaw = 2.0;
            follow.state.rig.pitch = 0.5;
        }
        let (mut current, mut shake, mut warned) = (None, None, HashSet::new());
        let batch = [
            GameCommand::SetFollowTarget {
                camera_id: "plain".into(),
                target_id: "new".into(),
            },
            GameCommand::SetFollowTarget {
                camera_id: "camera-id".into(),
                target_id: "new".into(),
            },
        ];
        apply_camera_commands(&mut world, &mut current, &mut shake, &mut warned, &batch);
        assert!(warned.contains(&("plain".to_owned(), "set_follow_target")));
        let follow = world.get::<CameraFollow>(rig).unwrap();
        assert_eq!(follow.target, "new");
        assert_eq!(follow.state, ThirdPersonState::default());
    }

    #[test]
    fn retargeted_follow_reseeds_without_a_snap() {
        let mut world = World::new();
        spawn_target(&mut world, "old", Vec3::ZERO);
        spawn_target(&mut world, "new", Vec3::new(20.0, 0.0, 0.0));
        let start = Vec3::new(0.0, 2.0, 6.0);
        let rig = spawn_follower(
            &mut world,
            CameraFollow::new("old", engine_ecs::components::FollowMode::THIRD_PERSON),
            start,
        );
        let (mut current, mut shake, mut warned) = (None, None, HashSet::new());
        apply_camera_commands(
            &mut world,
            &mut current,
            &mut shake,
            &mut warned,
            &[GameCommand::SetFollowTarget {
                camera_id: "camera-id".into(),
                target_id: "new".into(),
            }],
        );
        let input = FollowInput {
            dt: 1.0e-6,
            ..FollowInput::default()
        };
        let view =
            present_in_world(&mut world, &mut current, &[rig], 1.0, &input, &mut warned).unwrap();
        // One tiny step from the current pose: the eye has not jumped to the
        // new target's side.
        assert!((view.eye - start).length() < 0.05, "{:?}", view.eye);
    }

    #[test]
    fn near_vertical_look_keeps_the_previous_rotation() {
        let mut settings = third();
        settings.pitch_max = 89.9_f32.to_radians();
        let mut state = rig_state();
        let (mut eye, mut rotation);
        let mut last = Quat::IDENTITY;
        for step in 0..120 {
            let orbit = if step < 5 {
                Vec2::new(0.0, 1.0e6)
            } else {
                Vec2::ZERO
            };
            (eye, rotation) = follow_third_person(&mut state, Vec3::ZERO, orbit, 0.016, &settings);
            assert!(eye.is_finite());
            assert!(
                step < 5 || rotation.dot(last).abs() > 0.9,
                "rotation flipped at step {step}"
            );
            last = rotation;
        }
        assert!(state.rotation != Quat::IDENTITY);
    }

    fn shake_world() -> (World, Entity, engine_utils::Transform) {
        let mut world = World::new();
        let camera = spawn_object(&mut world, "a", true, 0.0);
        let before = world.get::<Transform>(camera).unwrap().0;
        (world, camera, before)
    }

    #[test]
    fn shake_decays_to_zero_and_never_moves_the_object() {
        let (mut world, camera, before) = shake_world();
        let mut current = Some(camera);
        let (mut shake, mut warned) = (None, HashSet::new());
        apply_camera_commands(
            &mut world,
            &mut current,
            &mut shake,
            &mut warned,
            &[GameCommand::CameraShake {
                strength: 1.0,
                seconds: 0.5,
            }],
        );
        let mut rng = engine_utils::Rng::new(7);
        let base = present(&mut world, &mut current, &[camera], 1.0).unwrap();
        let mut elapsed = 0.0;
        let mut max_seen: f32 = 0.0;
        let mut last = base;
        for _ in 0..30 {
            let dt = 1.0 / 60.0;
            elapsed += dt;
            last = apply_shake(base, &mut shake, &mut rng, dt);
            let offset = (last.eye - base.eye).length();
            assert!(offset <= 1.0 + 1e-4, "offset {offset} at {elapsed}");
            // Eye and target move together.
            assert!(((last.target - base.target) - (last.eye - base.eye)).length() < 1e-4);
            max_seen = max_seen.max(offset);
            assert_eq!(world.get::<Transform>(camera).unwrap().0, before);
        }
        assert!(max_seen > 0.5, "shake was visible: {max_seen}");
        assert!((last.eye - base.eye).length() < 1e-4, "ended at zero");
        assert!(shake.is_none(), "finished shake is dropped");
    }

    #[test]
    fn new_shake_replaces_the_current_one() {
        let (mut world, camera, _) = shake_world();
        let mut current = Some(camera);
        let (mut shake, mut warned) = (None, HashSet::new());
        let batch = [
            GameCommand::CameraShake {
                strength: 1.0,
                seconds: 5.0,
            },
            GameCommand::CameraShake {
                strength: 0.25,
                seconds: 1.0,
            },
        ];
        apply_camera_commands(&mut world, &mut current, &mut shake, &mut warned, &batch);
        let s = shake.unwrap();
        assert_eq!((s.strength, s.total, s.remaining), (0.25, 1.0, 1.0));
    }

    #[test]
    fn spawn_then_switch_in_one_batch_works() {
        // The ordered camera pass runs after spawns, so the new camera exists.
        let mut world = World::new();
        let a = spawn_object(&mut world, "a", true, 0.0);
        let commands = [
            GameCommand::SpawnEmpty {
                object_id: "00000000-0000-4000-8000-000000000009".into(),
                name: "cam".into(),
                position: [4.0, 0.0, 0.0],
            },
            set_camera("00000000-0000-4000-8000-000000000009"),
        ];
        crate::gameplay::apply_world_commands(&mut world, &commands, &mut |_, _, _, _| {
            Err(crate::gameplay::GameplayError::Prefab(
                "x".into(),
                "no".into(),
            ))
        })
        .unwrap();
        // Give the spawned object a camera (a prefab would have carried one).
        let spawned =
            crate::gameplay::find_object(&world, "00000000-0000-4000-8000-000000000009").unwrap();
        world.entity_mut(spawned).insert(CameraComponent(cam()));
        let mut current = Some(a);
        let (mut shake, mut warned) = (None, HashSet::new());
        apply_camera_commands(&mut world, &mut current, &mut shake, &mut warned, &commands);
        assert_eq!(current, Some(spawned));
        assert!(warned.is_empty());
    }

    #[test]
    fn disabled_cameras_are_never_chosen() {
        let mut world = World::new();
        let a = spawn_cam(&mut world, true, 0.0);
        let b = spawn_cam(&mut world, false, 0.0);
        world.entity_mut(a).insert(engine_ecs::components::Disabled);
        assert_eq!(choose_active_camera(&mut world, &[a, b]), Some(b));
        world.entity_mut(b).insert(engine_ecs::components::Disabled);
        assert_eq!(choose_active_camera(&mut world, &[a, b]), None);
    }

    #[test]
    fn shake_seconds_are_limited_to_ten() {
        let (mut world, camera, _) = shake_world();
        let mut current = Some(camera);
        let run = |seconds: f32, world: &mut World, current: &mut Option<Entity>| {
            let mut shake = None;
            let mut warned = HashSet::new();
            apply_camera_commands(
                world,
                current,
                &mut shake,
                &mut warned,
                &[GameCommand::CameraShake {
                    strength: 1.0,
                    seconds,
                }],
            );
            shake
        };
        let long = run(1.0e9, &mut world, &mut current).unwrap();
        assert_eq!((long.total, long.remaining), (10.0, 10.0));
        assert!(run(f32::INFINITY, &mut world, &mut current).is_none());
        assert!(run(f32::NAN, &mut world, &mut current).is_none());
    }

    #[test]
    fn exactly_degenerate_look_keeps_the_previous_rotation() {
        // Distance 0 and height 0 put the eye exactly on the look point.
        let settings = ThirdPersonSettings {
            distance: 0.0,
            height: 0.0,
            smoothing: 0.0,
            pitch_min: (-60.0_f32).to_radians(),
            pitch_max: 70.0_f32.to_radians(),
        };
        let kept = Quat::from_rotation_y(0.7);
        let mut state = rig_state();
        state.rotation = kept;
        let (eye, rotation) = follow_third_person(
            &mut state,
            Vec3::new(1.0, 2.0, 3.0),
            Vec2::ZERO,
            0.016,
            &settings,
        );
        assert!(near(eye, Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(rotation, kept);
        assert_eq!(state.rotation, kept);
    }

    #[test]
    fn shake_moves_the_picture_but_not_the_listener() {
        // `present_camera` takes the listener from the steady view and only
        // then shakes the picture; both steps are GPU-free.
        let steady =
            engine_renderer::Camera::new(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 2.0, 2.0), 1.0);
        let listener = listener_transform(&steady);
        let mut shake = Some(Shake {
            strength: 1.0,
            total: 1.0,
            remaining: 1.0,
        });
        let mut rng = engine_utils::Rng::new(3);
        let shaken = apply_shake(steady, &mut shake, &mut rng, 0.1);
        assert!((shaken.eye - steady.eye).length() > 0.1);
        assert_eq!(listener.translation, steady.eye);
        assert!(near(listener.translation, Vec3::new(1.0, 2.0, 3.0)));
    }

    #[test]
    fn paused_camera_does_not_shake_or_follow() {
        // `present_camera` passes dt = 0 while the simulation is paused.
        assert_eq!(super::super::state::camera_dt(true, 0.5), 0.0);
        assert_eq!(super::super::state::camera_dt(false, 0.5), 0.5);
        let steady =
            engine_renderer::Camera::new(Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 2.0, 2.0), 1.0);
        let mut shake = Some(Shake {
            strength: 1.0,
            total: 1.0,
            remaining: 0.6,
        });
        let mut rng = engine_utils::Rng::new(3);
        for _ in 0..10 {
            apply_shake(
                steady,
                &mut shake,
                &mut rng,
                super::super::state::camera_dt(true, 0.1),
            );
        }
        assert_eq!(shake.map(|s| s.remaining), Some(0.6));
    }
}
