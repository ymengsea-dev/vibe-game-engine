//! Physics through the [`Game`] API, with no direct simulation calls.
//!
//! A dynamic box dropped onto a static floor. The engine's frame loop
//! steps the physics world and writes the simulated pose into each body's
//! `Transform` once per fixed update, so the box falls, bounces, and
//! settles without this file ever calling `PhysicsWorld::step` or
//! `sync_rigid_bodies`.
//!
//! It also demonstrates audio the same way: the box carries an
//! `AudioEmitter` and the camera an `AudioListener`, so the impact sound
//! is positioned, played and kept in sync by the loop — no
//! `AudioContext::play_static` call anywhere in this file.
//!
//! ```sh
//! cargo run -p engine --example falling_body
//! ```
//!
//! Escape exits. The console prints the box's height once a second so the
//! simulation is observable without staring at the window.

use engine::prelude::*;
use glam::Vec3;

/// A short synthesised thud, so the example needs no asset file.
fn impact_sound() -> Option<StaticSound> {
    const RATE: u32 = 44_100;
    const SECONDS: f32 = 0.25;
    let sample_count = (RATE as f32 * SECONDS) as u32;

    // A decaying 90 Hz sine: reads as a low knock.
    let mut samples = Vec::with_capacity(sample_count as usize * 2);
    for i in 0..sample_count {
        let t = i as f32 / RATE as f32;
        let envelope = (1.0 - t / SECONDS).max(0.0).powi(3);
        let value = (t * 90.0 * std::f32::consts::TAU).sin() * envelope * 0.6;
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

    match StaticSound::from_bytes(wav, false) {
        Ok(sound) => Some(sound),
        Err(err) => {
            tracing::warn!(error = %err, "could not build the impact sound");
            None
        }
    }
}

/// Half-extent of the falling box, in world units.
const BOX_HALF: f32 = 0.5;

/// Where the box starts.
const DROP_HEIGHT: f32 = 6.0;

struct FallingBody {
    /// The falling box, so `update` can read its synced transform.
    box_entity: Option<Entity>,
    /// Seconds since the last height report.
    since_report: f32,
    /// Set once the landing sound has been triggered.
    landed: bool,
}

impl Game for FallingBody {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        // Static floor: a fixed body, so it never moves and nothing has
        // to drive it.
        ctx.spawn_body(
            RigidBodyBuilder::fixed().translation(Vec3::ZERO),
            ColliderBuilder::cuboid(8.0, 0.5, 8.0),
        );

        // The falling box. `spawn_body` inserts the rigid body and
        // collider into the physics world and gives the entity the
        // components that tie it to them.
        self.box_entity = Some(ctx.spawn_body(
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, DROP_HEIGHT, 0.0)),
            ColliderBuilder::cuboid(BOX_HALF, BOX_HALF, BOX_HALF),
        ));

        // The camera hears. Its transform drives kira's listener, so
        // panning follows the view.
        let eye = Vec3::new(6.0, 4.0, 8.0);
        ctx.spawn((
            engine::ecs::components::Transform::from(engine_utils::Transform::from_translation(
                eye,
            )),
            engine::ecs::components::GlobalTransform::default(),
            AudioListener::default(),
        ));

        // The box emits. `silent` because it should sound on impact, not
        // at spawn; the loop plays it when `play()` is called.
        if let Some(sound) = impact_sound()
            && let Some(entity) = self.box_entity
        {
            ctx.world_mut().entity_mut(entity).insert((
                engine::ecs::components::GlobalTransform::default(),
                AudioEmitter::silent(sound),
            ));
        }
        if !ctx.has_audio() {
            tracing::warn!("no audio device; the demo runs silent");
        }

        // Look at where the action is.
        *ctx.camera_mut() = Camera::new(
            Vec3::new(6.0, 4.0, 8.0),
            Vec3::new(0.0, 1.0, 0.0),
            ctx.camera_mut().aspect_ratio,
        );

        tracing::info!(
            drop_height = DROP_HEIGHT,
            "dropping a box onto a static floor"
        );
        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if ctx.input().is_key_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }

        // The transform read here is the pose from the end of the most
        // recent fixed step — the engine synced it for us.
        // Trigger the impact once, the first time the box is near rest.
        if !self.landed
            && let Some(entity) = self.box_entity
            && let Some(transform) = ctx
                .world()
                .get::<engine::ecs::components::Transform>(entity)
            && transform.0.translation.y < 1.1
        {
            self.landed = true;
            if let Some(mut emitter) = ctx.world_mut().get_mut::<AudioEmitter>(entity) {
                emitter.play();
                tracing::info!("box landed; impact sound triggered");
            }
        }

        self.since_report += dt;
        if self.since_report >= 1.0 {
            self.since_report = 0.0;
            if let Some(entity) = self.box_entity
                && let Some(transform) = ctx
                    .world()
                    .get::<engine::ecs::components::Transform>(entity)
            {
                tracing::info!(height = transform.0.translation.y, "box height");
            }

            // Raycasts read the same world, straight off the context.
            // Note this runs in `update`, not `setup`: rapier's query
            // structures are only populated by a step, so a raycast
            // before the first fixed step always reports a miss.
            match cast_ray(
                ctx.physics(),
                Vec3::new(0.0, 20.0, 0.0),
                Vec3::NEG_Y,
                100.0,
                None,
            ) {
                Ok(Some(hit)) => {
                    tracing::info!(distance = hit.distance, "ray hit the stack below")
                }
                Ok(None) => tracing::info!("ray hit nothing"),
                Err(err) => tracing::warn!(error = %err, "raycast rejected"),
            }
        }
    }
}

fn main() -> Result<(), GameError> {
    // Best-effort: a logging backend already installed by the host is not
    // a reason to refuse to run.
    let _ = logging::init_default();

    run_game(
        GameConfig::new("Falling Body", 1280, 720),
        FallingBody {
            box_entity: None,
            since_report: 0.0,
            landed: false,
        },
    )
}
