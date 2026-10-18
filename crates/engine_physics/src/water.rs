//! Deterministic water-surface sampling and buoyancy math.

use glam::{Vec2, Vec3};

use crate::PhysicsError;

/// One travelling analytic wave on a horizontal water surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterWave {
    /// XZ travel direction. A zero vector disables horizontal travel.
    pub direction: Vec2,
    /// Vertical displacement in world units.
    pub amplitude: f32,
    /// Crest-to-crest distance in world units.
    pub length: f32,
    /// Angular travel speed in radians per second.
    pub speed: f32,
}

impl Default for WaterWave {
    fn default() -> Self {
        Self {
            direction: Vec2::X,
            amplitude: 0.08,
            length: 4.0,
            speed: 0.8,
        }
    }
}

/// A bounded, horizontal water surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterSurface {
    /// XZ rectangle centre and base Y height.
    pub center: Vec3,
    /// Inclusive half-width and half-depth in world units.
    pub half_extent: Vec2,
    /// Still-water height before wave displacement.
    pub base_height: f32,
    /// Animated wave settings.
    pub wave: WaterWave,
    /// Surface flow velocity in world units per second.
    pub current: Vec3,
    /// Water mass density in kilograms per cubic metre.
    pub density: f32,
    /// Linear water drag coefficient.
    pub linear_drag: f32,
}

impl Default for WaterSurface {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            half_extent: Vec2::splat(10.0),
            base_height: 0.0,
            wave: WaterWave::default(),
            current: Vec3::ZERO,
            density: 1_000.0,
            linear_drag: 1.0,
        }
    }
}

/// Water values sampled at one world position and time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterSample {
    /// Animated world-space water height.
    pub surface_height: f32,
    /// Upward-facing unit normal.
    pub normal: Vec3,
    /// Water flow velocity at this point.
    pub current: Vec3,
    /// Water height minus sampled point height.
    pub depth: f32,
    /// Density inherited from the sampled surface.
    pub density: f32,
    /// Drag coefficient inherited from the sampled surface.
    pub linear_drag: f32,
}

/// Per-body buoyancy settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuoyancySettings {
    /// Displaced volume at full immersion in cubic metres.
    pub volume: f32,
    /// Material density in kilograms per cubic metre.
    pub density: f32,
    /// Offset from the rigid-body origin where water is sampled.
    pub center_offset: Vec3,
    /// Extra angular damping while submerged.
    pub angular_damping: f32,
}

impl Default for BuoyancySettings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            density: 500.0,
            center_offset: Vec3::ZERO,
            angular_damping: 1.0,
        }
    }
}

impl WaterSurface {
    /// Creates a water surface after validating every public input.
    pub fn new(
        center: Vec3,
        half_extent: Vec2,
        base_height: f32,
        wave: WaterWave,
        current: Vec3,
        density: f32,
        linear_drag: f32,
    ) -> Result<Self, PhysicsError> {
        validate_vec3("center", center)?;
        validate_positive("half_extent.x", half_extent.x)?;
        validate_positive("half_extent.y", half_extent.y)?;
        validate_finite("base_height", base_height)?;
        validate_vec2("wave.direction", wave.direction)?;
        validate_non_negative("wave.amplitude", wave.amplitude)?;
        validate_positive("wave.length", wave.length)?;
        validate_finite("wave.speed", wave.speed)?;
        validate_vec3("current", current)?;
        validate_positive("density", density)?;
        validate_non_negative("linear_drag", linear_drag)?;

        Ok(Self {
            center,
            half_extent,
            base_height,
            wave,
            current,
            density,
            linear_drag,
        })
    }

    /// Samples the surface at `world_position` and `time`.
    ///
    /// Returns `None` outside its inclusive XZ bounds or for non-finite
    /// query inputs.
    pub fn sample(&self, world_position: Vec3, time: f32) -> Option<WaterSample> {
        if !world_position.is_finite() || !time.is_finite() {
            return None;
        }
        let local = world_position - self.center;
        if local.x.abs() > self.half_extent.x || local.z.abs() > self.half_extent.y {
            return None;
        }

        let (height_offset, slope) =
            wave_displacement_and_slope(self.wave, Vec2::new(local.x, local.z), time);
        let surface_height = self.base_height + height_offset;
        Some(WaterSample {
            surface_height,
            normal: Vec3::new(-slope.x, 1.0, -slope.y).normalize_or_zero(),
            current: self.current,
            depth: surface_height - world_position.y,
            density: self.density,
            linear_drag: self.linear_drag,
        })
    }
}

/// Returns buoyant lift for `settings` at `sample` under `gravity`.
///
/// The characteristic immersion height is the cube root of the displaced
/// volume, keeping this geometry-free helper deterministic. Bodies with a
/// more detailed volume model can supply their own forces instead.
pub fn buoyancy_force(sample: WaterSample, settings: BuoyancySettings, gravity: Vec3) -> Vec3 {
    if !valid_buoyancy_settings(settings) || !gravity.is_finite() || sample.depth <= 0.0 {
        return Vec3::ZERO;
    }
    let immersion = (sample.depth / settings.volume.cbrt()).clamp(0.0, 1.0);
    let gravity_magnitude = gravity.length();
    if gravity_magnitude == 0.0 {
        return Vec3::ZERO;
    }
    -gravity / gravity_magnitude * sample.density * settings.volume * gravity_magnitude * immersion
}

fn wave_displacement_and_slope(wave: WaterWave, local_position: Vec2, time: f32) -> (f32, Vec2) {
    let direction = wave.direction.normalize_or_zero();
    if direction == Vec2::ZERO || wave.amplitude == 0.0 {
        return (0.0, Vec2::ZERO);
    }
    let wave_number = std::f32::consts::TAU / wave.length;
    let phase = direction.dot(local_position) * wave_number + wave.speed * time;
    let slope = direction * (wave.amplitude * wave_number * phase.cos());
    (wave.amplitude * phase.sin(), slope)
}

fn valid_buoyancy_settings(settings: BuoyancySettings) -> bool {
    settings.volume.is_finite()
        && settings.volume > 0.0
        && settings.density.is_finite()
        && settings.density > 0.0
        && settings.center_offset.is_finite()
        && settings.angular_damping.is_finite()
        && settings.angular_damping >= 0.0
}

fn validate_finite(field: &'static str, value: f32) -> Result<(), PhysicsError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PhysicsError::InvalidWaterSettings { field, value })
    }
}

fn validate_positive(field: &'static str, value: f32) -> Result<(), PhysicsError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(PhysicsError::InvalidWaterSettings { field, value })
    }
}

fn validate_non_negative(field: &'static str, value: f32) -> Result<(), PhysicsError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(PhysicsError::InvalidWaterSettings { field, value })
    }
}

fn validate_vec2(field: &'static str, value: Vec2) -> Result<(), PhysicsError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PhysicsError::InvalidWaterSettings {
            field,
            value: f32::NAN,
        })
    }
}

fn validate_vec3(field: &'static str, value: Vec3) -> Result<(), PhysicsError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PhysicsError::InvalidWaterSettings {
            field,
            value: f32::NAN,
        })
    }
}
