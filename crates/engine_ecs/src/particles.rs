//! CPU particle simulation: emitters that spawn, integrate, age, and
//! recycle a fixed pool of particles each frame, plus extraction of the
//! live set into `engine_renderer::ParticleInstance`s for GPU-instanced
//! billboard rendering.
//!
//! The renderer (`engine_renderer`'s particle pipelines) owns only the
//! draw; this module owns the motion. [`update_particles`] advances every
//! [`ParticleEmitter`] in the world by a timestep; [`extract_particles`]
//! turns their live particles into the two blend-mode instance slices
//! [`engine_renderer::ParticleFrame`] wants.
//!
//! Simulation is deterministic: every emitter carries its own seeded
//! [`Rng`] (SplitMix64), so a given `seed` + timestep sequence always
//! produces the same particles. No `rand` dependency.
//!
//! Deliberately minimal for one iteration: one emission shape (a cone),
//! linear size/colour interpolation over life, constant acceleration.
//! Textured/animated particles, burst emission, per-particle drag,
//! attractors, sorting for exact alpha order, and GPU-compute simulation
//! are all future work.

use bevy_ecs::prelude::Component;
use bevy_ecs::world::World;
use engine_renderer::ParticleInstance;
use glam::Vec3;

use crate::components::GlobalTransform;
use crate::propagate::propagate_transforms;

/// Re-exported from `engine_utils` (where it moved once
/// `engine_renderer::scatter` needed the same generator). `engine_ecs::Rng`
/// stays a valid path.
pub use engine_utils::Rng;

/// How an emitter's particles composite. Selects which
/// `engine_renderer::ParticlePipeline` draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// Standard transparency. Order-dependent (the MVP draws in pool
    /// order); good for smoke, dust, mist.
    Alpha,
    /// Additive. Order-independent, blooms nicely; good for fire, sparks,
    /// embers, magic.
    Additive,
}

/// Immutable emission parameters for a [`ParticleEmitter`]. Every field is
/// world-space unless noted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleEmitterConfig {
    /// Particles spawned per second (fractional rates accumulate).
    pub spawn_rate: f32,
    /// Base particle lifetime, seconds.
    pub lifetime: f32,
    /// Uniform +/- jitter added to `lifetime` at spawn, seconds.
    pub lifetime_jitter: f32,
    /// Base launch speed along the emission direction.
    pub initial_speed: f32,
    /// Uniform +/- jitter added to `initial_speed` at spawn.
    pub speed_jitter: f32,
    /// Central emission direction (normalized internally).
    pub direction: Vec3,
    /// Cone half-angle around `direction`, radians. `0.0` emits a straight
    /// stream.
    pub spread: f32,
    /// Constant acceleration applied every step (gravity, buoyancy, wind).
    pub acceleration: Vec3,
    /// Billboard edge length at birth.
    pub start_size: f32,
    /// Billboard edge length at death (linearly interpolated over life).
    pub end_size: f32,
    /// Linear RGBA at birth.
    pub start_color: [f32; 4],
    /// Linear RGBA at death (linearly interpolated over life).
    pub end_color: [f32; 4],
    /// Hard cap on live particles for this emitter — bounds its pool
    /// allocation and per-frame work.
    pub max_particles: usize,
    /// Which pipeline draws this emitter's particles.
    pub blend: BlendMode,
    /// Seed for this emitter's [`Rng`].
    pub seed: u64,
}

impl ParticleEmitterConfig {
    /// A warm additive ember fountain: rises, drifts down slightly, fades
    /// orange to dark red, ~200 particles. A reasonable starting point to
    /// tweak from.
    pub const EMBERS: Self = Self {
        spawn_rate: 60.0,
        lifetime: 1.6,
        lifetime_jitter: 0.4,
        initial_speed: 1.4,
        speed_jitter: 0.5,
        direction: Vec3::Y,
        spread: 0.35,
        acceleration: Vec3::new(0.0, -0.6, 0.0),
        start_size: 0.09,
        end_size: 0.02,
        start_color: [1.0, 0.55, 0.15, 1.0],
        end_color: [0.5, 0.06, 0.0, 0.0],
        max_particles: 200,
        blend: BlendMode::Additive,
        seed: 0x5645_4745_4D42_5253,
    };
}

impl Default for ParticleEmitterConfig {
    /// [`ParticleEmitterConfig::EMBERS`].
    fn default() -> Self {
        Self::EMBERS
    }
}

/// One live particle. Private — emitters own their pool; the outside world
/// sees only [`ParticleEmitter::len`] and the extracted instances.
#[derive(Debug, Clone, Copy)]
struct Particle {
    position: Vec3,
    velocity: Vec3,
    age: f32,
    lifetime: f32,
    rotation: f32,
    rotation_speed: f32,
}

impl Particle {
    fn is_dead(&self) -> bool {
        self.age >= self.lifetime
    }

    /// Normalized age in `[0, 1]` — the interpolation parameter for size
    /// and colour. `lifetime` is always `>= MIN_LIFETIME`, so this never
    /// divides by zero.
    fn life_fraction(&self) -> f32 {
        (self.age / self.lifetime).clamp(0.0, 1.0)
    }
}

/// Smallest lifetime a spawned particle can have, so `life_fraction`'s
/// division is always safe.
const MIN_LIFETIME: f32 = 1.0e-3;

/// Advances one particle: `velocity += acceleration * dt`, then
/// `position += velocity * dt`, plus roll and age. Pure; `dt` is assumed
/// finite and non-negative (callers clamp).
fn step_particle(particle: &mut Particle, acceleration: Vec3, dt: f32) {
    particle.velocity += acceleration * dt;
    particle.position += particle.velocity * dt;
    particle.rotation += particle.rotation_speed * dt;
    particle.age += dt;
}

/// Builds one fresh particle at `origin`, drawing every random choice from
/// `rng` so the result is deterministic for a given generator state.
fn spawn_particle(config: &ParticleEmitterConfig, origin: Vec3, rng: &mut Rng) -> Particle {
    let direction = rng.cone_direction(config.direction, config.spread);
    let speed =
        (config.initial_speed + rng.range(-config.speed_jitter, config.speed_jitter)).max(0.0);
    let lifetime = (config.lifetime + rng.range(-config.lifetime_jitter, config.lifetime_jitter))
        .max(MIN_LIFETIME);
    Particle {
        position: origin,
        velocity: direction * speed,
        age: 0.0,
        lifetime,
        rotation: rng.range(0.0, core::f32::consts::TAU),
        // Gentle random spin, +/- ~1 rad/s.
        rotation_speed: rng.signed_unit(),
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp(a[0], b[0], t),
        lerp(a[1], b[1], t),
        lerp(a[2], b[2], t),
        lerp(a[3], b[3], t),
    ]
}

/// An entity component that continuously emits particles from its
/// [`GlobalTransform`] position.
///
/// Add one to an entity, then call [`update_particles`] once per frame (or
/// per fixed step) and pass [`extract_particles`]' output into
/// `engine_renderer` via `ParticleFrame`. The pool, RNG, and spawn
/// accumulator are internal; `config` is public so it can be tweaked at
/// runtime (changing `seed` mid-run does not reset the live `Rng`).
#[derive(Component, Debug, Clone)]
pub struct ParticleEmitter {
    /// Emission parameters. Safe to mutate between frames.
    pub config: ParticleEmitterConfig,
    particles: Vec<Particle>,
    rng: Rng,
    spawn_accumulator: f32,
}

impl ParticleEmitter {
    /// A new emitter with `config`, its pool empty and its [`Rng`] seeded
    /// from `config.seed`.
    pub fn new(config: ParticleEmitterConfig) -> Self {
        Self {
            rng: Rng::new(config.seed),
            particles: Vec::with_capacity(config.max_particles.min(4096)),
            spawn_accumulator: 0.0,
            config,
        }
    }

    /// How many particles are currently alive.
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    /// Whether no particles are currently alive.
    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// Advances this emitter `dt` seconds with its origin at `origin`:
    /// integrates and ages every live particle, drops the dead ones, then
    /// spawns new particles at `config.spawn_rate` (never exceeding
    /// `config.max_particles`).
    ///
    /// A non-positive or non-finite `dt` is a no-op — the simulation only
    /// ever moves forward.
    pub fn update(&mut self, origin: Vec3, dt: f32) {
        if dt <= 0.0 || !dt.is_finite() {
            return;
        }

        let acceleration = self.config.acceleration;
        for particle in &mut self.particles {
            step_particle(particle, acceleration, dt);
        }
        self.particles.retain(|particle| !particle.is_dead());

        let capacity = self.config.max_particles;
        if self.particles.len() >= capacity {
            // Already full: don't let the accumulator bank a burst for
            // when particles start dying.
            self.spawn_accumulator = 0.0;
            return;
        }

        self.spawn_accumulator += self.config.spawn_rate.max(0.0) * dt;
        while self.spawn_accumulator >= 1.0 {
            self.spawn_accumulator -= 1.0;
            if self.particles.len() >= capacity {
                self.spawn_accumulator = 0.0;
                break;
            }
            let particle = spawn_particle(&self.config, origin, &mut self.rng);
            self.particles.push(particle);
        }
    }

    /// Appends this emitter's live particles to `out` as GPU instances,
    /// with size and colour interpolated over each particle's normalized
    /// age.
    pub fn extract_into(&self, out: &mut Vec<ParticleInstance>) {
        out.reserve(self.particles.len());
        for particle in &self.particles {
            let t = particle.life_fraction();
            out.push(ParticleInstance {
                position: particle.position.to_array(),
                size: lerp(self.config.start_size, self.config.end_size, t),
                color: lerp4(self.config.start_color, self.config.end_color, t),
                rotation: particle.rotation,
                _padding: [0.0; 3],
            });
        }
    }
}

/// Advances every [`ParticleEmitter`] in `world` by `dt` seconds, each from
/// its entity's current world-space position.
///
/// Runs [`propagate_transforms`] first so emitters parented into a
/// hierarchy emit from the right place. Call once per frame (with the real
/// frame time, clamped) or once per fixed step, before rendering.
pub fn update_particles(world: &mut World, dt: f32) {
    propagate_transforms(world);
    let mut query = world.query::<(&GlobalTransform, &mut ParticleEmitter)>();
    for (transform, mut emitter) in query.iter_mut(world) {
        emitter.update(transform.0.translation, dt);
    }
}

/// Collects every [`ParticleEmitter`]'s live particles into two
/// `engine_renderer::ParticleInstance` lists — `(alpha, additive)` — ready
/// for `engine_renderer::ParticleFrame`. Emitters contribute to whichever
/// list their `config.blend` selects.
///
/// Does not simulate — call [`update_particles`] first.
pub fn extract_particles(world: &mut World) -> (Vec<ParticleInstance>, Vec<ParticleInstance>) {
    let mut alpha = Vec::new();
    let mut additive = Vec::new();
    let mut query = world.query::<&ParticleEmitter>();
    for emitter in query.iter(&*world) {
        match emitter.config.blend {
            BlendMode::Alpha => emitter.extract_into(&mut alpha),
            BlendMode::Additive => emitter.extract_into(&mut additive),
        }
    }
    (alpha, additive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cone_direction_still_reachable_through_the_re_export() {
        // `Rng` (with `cone_direction`) now lives in `engine_utils`; this
        // just pins that `engine_ecs::particles::Rng` resolves and the
        // method is usable here.
        let mut rng = Rng::new(11);
        let dir = rng.cone_direction(Vec3::Y, 0.3);
        assert!((dir.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn step_particle_integrates_velocity_then_position() {
        let mut p = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::new(1.0, 0.0, 0.0),
            age: 0.0,
            lifetime: 10.0,
            rotation: 0.0,
            rotation_speed: 2.0,
        };
        step_particle(&mut p, Vec3::new(0.0, -10.0, 0.0), 0.5);
        // velocity: (1, 0, 0) + (0, -10, 0) * 0.5 = (1, -5, 0)
        assert_eq!(p.velocity, Vec3::new(1.0, -5.0, 0.0));
        // position: 0 + (1, -5, 0) * 0.5 = (0.5, -2.5, 0)
        assert_eq!(p.position, Vec3::new(0.5, -2.5, 0.0));
        assert_eq!(p.age, 0.5);
        assert_eq!(p.rotation, 1.0);
    }

    #[test]
    fn particle_is_dead_at_or_past_its_lifetime() {
        let mut p = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.9,
            lifetime: 1.0,
            rotation: 0.0,
            rotation_speed: 0.0,
        };
        assert!(!p.is_dead());
        step_particle(&mut p, Vec3::ZERO, 0.1);
        assert!(p.is_dead());
        assert_eq!(p.life_fraction(), 1.0);
    }

    fn test_config() -> ParticleEmitterConfig {
        ParticleEmitterConfig {
            spawn_rate: 10.0,
            lifetime: 1.0,
            lifetime_jitter: 0.0,
            initial_speed: 2.0,
            speed_jitter: 0.5,
            direction: Vec3::Y,
            spread: 0.2,
            acceleration: Vec3::ZERO,
            start_size: 1.0,
            end_size: 0.0,
            start_color: [1.0, 1.0, 1.0, 1.0],
            end_color: [0.0, 0.0, 0.0, 0.0],
            max_particles: 100,
            blend: BlendMode::Alpha,
            seed: 123,
        }
    }

    #[test]
    fn spawn_particle_starts_at_origin_within_speed_and_lifetime_bands() {
        let config = test_config();
        let mut rng = Rng::new(config.seed);
        let origin = Vec3::new(5.0, 1.0, -2.0);
        for _ in 0..500 {
            let p = spawn_particle(&config, origin, &mut rng);
            assert_eq!(p.position, origin);
            assert_eq!(p.age, 0.0);
            let speed = p.velocity.length();
            assert!((1.5..=2.5).contains(&speed), "speed {speed}");
            assert_eq!(p.lifetime, 1.0);
        }
    }

    #[test]
    fn spawn_particle_clamps_lifetime_above_zero() {
        let mut config = test_config();
        config.lifetime = 0.0;
        config.lifetime_jitter = 0.0;
        let mut rng = Rng::new(1);
        let p = spawn_particle(&config, Vec3::ZERO, &mut rng);
        assert!(p.lifetime >= MIN_LIFETIME);
    }

    #[test]
    fn emitter_update_spawns_at_the_configured_rate() {
        let mut emitter = ParticleEmitter::new(test_config());
        // 10 per second for 1 second, in 0.1s steps -> ~10 particles.
        for _ in 0..10 {
            emitter.update(Vec3::ZERO, 0.1);
        }
        assert_eq!(emitter.len(), 10);
    }

    #[test]
    fn emitter_update_removes_dead_particles() {
        let mut emitter = ParticleEmitter::new(test_config());
        for _ in 0..5 {
            emitter.update(Vec3::ZERO, 0.1);
        }
        let mid = emitter.len();
        assert!(mid > 0);
        // Push well past lifetime (1s) with no new spawns possible to
        // outpace deaths: one big step ages everything out.
        emitter.config.spawn_rate = 0.0;
        emitter.update(Vec3::ZERO, 2.0);
        assert_eq!(emitter.len(), 0);
    }

    #[test]
    fn emitter_update_never_exceeds_max_particles() {
        let mut config = test_config();
        config.spawn_rate = 10_000.0;
        config.max_particles = 50;
        config.lifetime = 100.0;
        let mut emitter = ParticleEmitter::new(config);
        for _ in 0..20 {
            emitter.update(Vec3::ZERO, 0.1);
        }
        assert_eq!(emitter.len(), 50);
    }

    #[test]
    fn emitter_update_ignores_nonpositive_and_nonfinite_dt() {
        let mut emitter = ParticleEmitter::new(test_config());
        emitter.update(Vec3::ZERO, 0.0);
        emitter.update(Vec3::ZERO, -1.0);
        emitter.update(Vec3::ZERO, f32::NAN);
        emitter.update(Vec3::ZERO, f32::INFINITY);
        assert_eq!(emitter.len(), 0);
    }

    #[test]
    fn extract_into_interpolates_size_and_colour_over_life() {
        let mut emitter = ParticleEmitter::new(test_config());
        // First step spawns one particle at age 0.
        emitter.update(Vec3::ZERO, 0.1);
        let mut fresh = Vec::new();
        emitter.extract_into(&mut fresh);
        assert_eq!(fresh.len(), 1);
        // t = 0: full start values.
        assert!((fresh[0].size - 1.0).abs() < 1e-5);
        assert!((fresh[0].color[3] - 1.0).abs() < 1e-5);

        // Age that particle to 0.1 / 1.0 without spawning another.
        emitter.config.spawn_rate = 0.0;
        emitter.update(Vec3::ZERO, 0.1);
        let mut aged = Vec::new();
        emitter.extract_into(&mut aged);
        assert_eq!(aged.len(), 1);
        // t = 0.1: size 1.0 -> 0.0 gives 0.9; colour alpha 1.0 -> 0.0
        // gives 0.9.
        assert!((aged[0].size - 0.9).abs() < 1e-5);
        assert!((aged[0].color[3] - 0.9).abs() < 1e-5);
    }

    #[test]
    fn extract_particles_splits_by_blend_mode() {
        let mut world = World::new();

        let mut alpha_cfg = test_config();
        alpha_cfg.blend = BlendMode::Alpha;
        let mut alpha_emitter = ParticleEmitter::new(alpha_cfg);
        alpha_emitter.update(Vec3::ZERO, 0.25); // ~2-3 particles

        let mut add_cfg = test_config();
        add_cfg.blend = BlendMode::Additive;
        add_cfg.seed = 999;
        let mut add_emitter = ParticleEmitter::new(add_cfg);
        add_emitter.update(Vec3::ZERO, 0.55); // ~5-6 particles

        let alpha_len = alpha_emitter.len();
        let add_len = add_emitter.len();
        world.spawn(alpha_emitter);
        world.spawn(add_emitter);

        let (alpha, additive) = extract_particles(&mut world);
        assert_eq!(alpha.len(), alpha_len);
        assert_eq!(additive.len(), add_len);
    }

    #[test]
    fn extract_particles_on_an_empty_world_is_empty() {
        let mut world = World::new();
        let (alpha, additive) = extract_particles(&mut world);
        assert!(alpha.is_empty() && additive.is_empty());
    }
}
