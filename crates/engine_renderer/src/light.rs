//! Directional, point and spot lights, and their GPU-uniform representation.
//!
//! Fixed-capacity arrays ([`MAX_DIRECTIONAL_LIGHTS`]/[`MAX_POINT_LIGHTS`])
//! rather than a dynamically-sized storage buffer — simpler (a plain
//! uniform buffer, no extra device feature checks) and plenty for a scene
//! with a handful of lights; revisit with a storage buffer if a scene
//! ever needs more.

use glam::Vec3;

/// How many directional lights [`LightSet::to_uniform`] uploads. Extra
/// lights beyond this are silently dropped — see [`LightSet::to_uniform`].
pub const MAX_DIRECTIONAL_LIGHTS: usize = 2;
/// How many point lights are uploaded per frame. With more in the scene,
/// [`LightSet::to_uniform_for_view`] keeps the ones that matter most to the
/// camera. Must match `MAX_POINT_LIGHTS` in `shaders/pbr_shared.wgsl`.
pub const MAX_POINT_LIGHTS: usize = 16;
/// How many spot lights are uploaded per frame, chosen like point lights.
/// Must match `MAX_SPOT_LIGHTS` in `shaders/pbr_shared.wgsl`.
pub const MAX_SPOT_LIGHTS: usize = 8;
/// How many point or spot lights cast shadows at once: the ones that
/// matter most to the camera among those with `casts_shadows`. Must match
/// `MAX_SHADOWED_LIGHTS` in `shaders/pbr_shared.wgsl`.
pub const MAX_SHADOWED_LIGHTS: usize = 4;
/// How far from the camera the sun casts shadows when a scene doesn't say.
pub const DEFAULT_SHADOW_DISTANCE: f32 = 60.0;

/// A light with parallel rays and no falloff (e.g. sunlight) — direction
/// only, no position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectionalLight {
    /// The direction the light travels *toward* (not the direction to
    /// the light source — e.g. `Vec3::NEG_Y` is a light shining straight
    /// down).
    pub direction: Vec3,
    /// Light color (linear RGB, typically `[0, 1]` per channel but not
    /// clamped — values above `1` are valid for an especially bright
    /// light).
    pub color: Vec3,
    /// Brightness multiplier.
    pub intensity: f32,
}

/// A light radiating from a single point in all directions, with
/// inverse-square distance falloff (e.g. a bulb).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointLight {
    /// World-space position.
    pub position: Vec3,
    /// Light color, same convention as [`DirectionalLight::color`].
    pub color: Vec3,
    /// Brightness multiplier.
    pub intensity: f32,
    /// Currently unused by the falloff calculation (plain inverse-square,
    /// no hard cutoff) — reserved for a future windowed-falloff model
    /// that fades to exactly zero at this distance instead of just
    /// getting very dim. Also how far its shadow reaches.
    pub range: f32,
    /// Whether it may cast shadows (see [`MAX_SHADOWED_LIGHTS`]).
    pub casts_shadows: bool,
}

impl PointLight {
    /// Approximate brightness this light adds at `point`: its strongest colour
    /// channel times intensity, with inverse-square falloff (softened so a
    /// light at the eye does not divide by zero).
    pub fn influence_at(&self, point: Vec3) -> f32 {
        influence(self.position, self.color, self.intensity, point)
    }
}

/// A light shining a cone from a point (e.g. a torch or a stage light). Its
/// brightness falls off with distance and fades out between the inner and
/// outer angles, reaching zero at `range`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotLight {
    /// World-space position.
    pub position: Vec3,
    /// The direction the cone points.
    pub direction: Vec3,
    /// Light color, same convention as [`DirectionalLight::color`].
    pub color: Vec3,
    /// Brightness multiplier.
    pub intensity: f32,
    /// Distance at which the light has faded to nothing.
    pub range: f32,
    /// Half-angle of the fully lit inner cone, in radians.
    pub inner_angle: f32,
    /// Half-angle of the outer cone, in radians; dark beyond it.
    pub outer_angle: f32,
    /// Whether it may cast shadows (see [`MAX_SHADOWED_LIGHTS`]).
    pub casts_shadows: bool,
}

impl SpotLight {
    /// Approximate brightness this light adds at `point`, as
    /// [`PointLight::influence_at`] (the cone is ignored).
    pub fn influence_at(&self, point: Vec3) -> f32 {
        influence(self.position, self.color, self.intensity, point)
    }

    /// How much of the light reaches `point`, in `[0, 1]`: the cone fade
    /// times the windowed distance falloff (without the inverse square).
    /// The CPU mirror of `spot_attenuation` in `pbr_shared.wgsl`.
    pub fn cone_and_range_at(&self, point: Vec3) -> f32 {
        let to_point = point - self.position;
        let distance = to_point.length();
        if distance <= f32::EPSILON {
            return 1.0;
        }
        let cos_angle = (to_point / distance).dot(self.direction.normalize_or_zero());
        let (cos_inner, cos_outer) = self.cone_cosines();
        let cone = smoothstep(cos_outer, cos_inner, cos_angle);
        cone * range_window(distance, self.range)
    }

    /// `(cos inner, cos outer)`, with the inner angle kept inside the outer.
    fn cone_cosines(&self) -> (f32, f32) {
        let outer = self
            .outer_angle
            .clamp(0.0, std::f32::consts::FRAC_PI_2 - 1e-3);
        let inner = self.inner_angle.clamp(0.0, outer);
        // A hair apart so the smoothstep never divides by zero.
        (inner.cos(), outer.cos().min(inner.cos() - 1e-4))
    }
}

fn influence(position: Vec3, color: Vec3, intensity: f32, point: Vec3) -> f32 {
    let brightness = intensity.max(0.0) * color.max_element().max(0.0);
    brightness / (1.0 + position.distance_squared(point))
}

/// `(1 - (d / range)^4)^2`, clamped: 1 near the light, 0 at `range`.
fn range_window(distance: f32, range: f32) -> f32 {
    if range <= 0.0 {
        return 0.0;
    }
    let ratio = (distance / range).powi(4);
    let window = (1.0 - ratio).clamp(0.0, 1.0);
    window * window
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A light chosen to cast shadows, by its index in [`LightSet::point`] or
/// [`LightSet::spot`]. Its place in [`LightSet::shadowed_lights`] is its
/// shadow slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowedLight {
    /// `LightSet::point[index]`.
    Point(usize),
    /// `LightSet::spot[index]`.
    Spot(usize),
}

/// Indirect light approximated as two hemispheres: one colour arriving
/// from the sky above, another bounced up off the ground below, mixed by
/// how much a surface faces up.
///
/// Not physically-based global illumination — it is the cheapest model
/// that still separates "in shade outdoors" from "in a cave". Before
/// this, the shader used a flat `albedo * 0.03`, which made every
/// surface not hit by the sun read as near-black and was the single
/// biggest reason scenes looked lifeless.
///
/// The blend is mirrored in `pbr_shared.wgsl`; see
/// [`AmbientLight::color_for_normal`], which is the CPU-side copy the
/// tests exercise. **The two must be kept in step** — there is no
/// compiler check tying them together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AmbientLight {
    /// Linear RGB arriving from straight up.
    pub sky_color: Vec3,
    /// Linear RGB bounced from straight down.
    pub ground_color: Vec3,
    /// Overall multiplier. `0.0` disables ambient entirely (every
    /// unlit surface goes black).
    pub intensity: f32,
}

impl AmbientLight {
    /// Outdoor daylight: a cool sky above, a warm earth bounce below.
    pub const DAYLIGHT: Self = Self {
        sky_color: Vec3::new(0.45, 0.58, 0.75),
        ground_color: Vec3::new(0.30, 0.26, 0.20),
        intensity: 1.0,
    };

    /// No indirect light at all — everything outside a direct light's
    /// reach renders black.
    pub const NONE: Self = Self {
        sky_color: Vec3::ZERO,
        ground_color: Vec3::ZERO,
        intensity: 0.0,
    };

    /// The ambient colour reaching a surface whose normal is `normal`.
    ///
    /// `normal.y` of `+1` (facing straight up) returns the full sky
    /// colour, `-1` the full ground colour, `0` an even mix — scaled by
    /// [`AmbientLight::intensity`].
    ///
    /// This is the CPU mirror of the WGSL in `pbr_shared.wgsl`. Keep both
    /// in step.
    pub fn color_for_normal(&self, normal: Vec3) -> Vec3 {
        let up_factor = (normal.normalize_or_zero().y * 0.5 + 0.5).clamp(0.0, 1.0);
        self.ground_color.lerp(self.sky_color, up_factor) * self.intensity.max(0.0)
    }
}

impl Default for AmbientLight {
    /// [`AmbientLight::DAYLIGHT`].
    fn default() -> Self {
        Self::DAYLIGHT
    }
}

/// The lights in a scene, ready to pack into a [`LightsUniform`].
#[derive(Debug, Clone, PartialEq)]
pub struct LightSet {
    /// Directional lights (e.g. one sun).
    pub directional: Vec<DirectionalLight>,
    /// Point lights.
    pub point: Vec<PointLight>,
    /// Spot lights.
    pub spot: Vec<SpotLight>,
    /// Indirect light. Defaults to [`AmbientLight::DAYLIGHT`].
    pub ambient: AmbientLight,
    /// How far from the camera the first directional light casts shadows.
    /// Defaults to [`DEFAULT_SHADOW_DISTANCE`].
    pub shadow_distance: f32,
}

impl Default for LightSet {
    fn default() -> Self {
        Self {
            directional: Vec::new(),
            point: Vec::new(),
            spot: Vec::new(),
            ambient: AmbientLight::default(),
            shadow_distance: DEFAULT_SHADOW_DISTANCE,
        }
    }
}

impl LightSet {
    /// A light set with no direct lights, lit only by
    /// [`AmbientLight::DAYLIGHT`].
    ///
    /// Not fully black: a scene whose lights haven't been authored yet
    /// reads as "outdoors in shade" rather than a void, which is almost
    /// always what someone wants while building one. Set
    /// [`LightSet::ambient`] to [`AmbientLight::NONE`] for genuine
    /// darkness.
    pub fn new() -> Self {
        Self::default()
    }

    /// Packs this light set for GPU upload.
    ///
    /// If `directional`/`point` holds more than
    /// [`MAX_DIRECTIONAL_LIGHTS`]/[`MAX_POINT_LIGHTS`] entries, only the
    /// first `MAX_*` are uploaded — the rest are silently dropped rather
    /// than erroring, since exceeding a soft rendering-resource cap isn't
    /// a correctness problem the way e.g. malformed asset data is.
    ///
    /// Shadow slots are chosen as [`Self::shadowed_lights`] would for a
    /// camera at the origin.
    pub fn to_uniform(&self) -> LightsUniform {
        self.pack(None)
    }

    /// Packs this light set for a camera at `eye`. When there are more point
    /// (or spot) lights than [`MAX_POINT_LIGHTS`] ([`MAX_SPOT_LIGHTS`]), the
    /// ones contributing most at the eye (brightness over squared distance)
    /// are kept; ties keep authored order. The shadow slots match
    /// [`Self::shadowed_lights`] for the same `eye`.
    pub fn to_uniform_for_view(&self, eye: Vec3) -> LightsUniform {
        self.pack(Some(eye))
    }

    /// The lights that cast shadows for a camera at `eye`: up to
    /// [`MAX_SHADOWED_LIGHTS`] of those with `casts_shadows`, the ones
    /// contributing most at the eye first (ties keep authored order, points
    /// before spots). A light's place in the list is its shadow slot.
    pub fn shadowed_lights(&self, eye: Vec3) -> Vec<ShadowedLight> {
        let mut candidates: Vec<(ShadowedLight, f32)> = self
            .point
            .iter()
            .enumerate()
            .filter(|(_, light)| light.casts_shadows)
            .map(|(index, light)| (ShadowedLight::Point(index), light.influence_at(eye)))
            .chain(
                self.spot
                    .iter()
                    .enumerate()
                    .filter(|(_, light)| light.casts_shadows)
                    .map(|(index, light)| (ShadowedLight::Spot(index), light.influence_at(eye))),
            )
            .collect();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
        candidates.truncate(MAX_SHADOWED_LIGHTS);
        candidates.into_iter().map(|(light, _)| light).collect()
    }

    fn pack(&self, eye: Option<Vec3>) -> LightsUniform {
        let shadowed = self.shadowed_lights(eye.unwrap_or(Vec3::ZERO));
        let slot_of = |light: ShadowedLight| {
            shadowed
                .iter()
                .position(|candidate| *candidate == light)
                .map_or(-1, |slot| slot as i32)
        };

        let mut directional_lights = [DirectionalLightUniform::ZERO; MAX_DIRECTIONAL_LIGHTS];
        for (slot, light) in directional_lights.iter_mut().zip(&self.directional) {
            *slot = DirectionalLightUniform::from(*light);
        }

        let points = kept(self.point.len(), MAX_POINT_LIGHTS, eye, |index, eye| {
            self.point[index].influence_at(eye)
        });
        let mut point_lights = [PointLightUniform::ZERO; MAX_POINT_LIGHTS];
        for (slot, &index) in point_lights.iter_mut().zip(&points) {
            *slot = PointLightUniform::from(self.point[index])
                .with_shadow_slot(slot_of(ShadowedLight::Point(index)));
        }

        let spots = kept(self.spot.len(), MAX_SPOT_LIGHTS, eye, |index, eye| {
            self.spot[index].influence_at(eye)
        });
        let mut spot_lights = [SpotLightUniform::ZERO; MAX_SPOT_LIGHTS];
        for (slot, &index) in spot_lights.iter_mut().zip(&spots) {
            *slot = SpotLightUniform::from(self.spot[index])
                .with_shadow_slot(slot_of(ShadowedLight::Spot(index)));
        }

        LightsUniform {
            directional_count: self.directional.len().min(MAX_DIRECTIONAL_LIGHTS) as u32,
            point_count: points.len() as u32,
            _padding: [0; 2],
            directional_lights,
            point_lights,
            ambient_sky: self.ambient.sky_color.to_array(),
            ambient_intensity: self.ambient.intensity.max(0.0),
            ambient_ground: self.ambient.ground_color.to_array(),
            spot_count: spots.len() as u32,
            spot_lights,
        }
    }
}

/// The indices of `len` lights to upload, at most `max`: authored order when
/// they all fit (or there is no `eye`), else the most influential first.
fn kept(
    len: usize,
    max: usize,
    eye: Option<Vec3>,
    influence: impl Fn(usize, Vec3) -> f32,
) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..len).collect();
    if let Some(eye) = eye
        && len > max
    {
        indices.sort_by(|a, b| influence(*b, eye).total_cmp(&influence(*a, eye)));
    }
    indices.truncate(max);
    indices
}

/// GPU-layout [`DirectionalLight`]. `_padding` keeps the struct
/// 16-byte-aligned (a WGSL uniform-address-space array-element
/// requirement) without needing a `vec4` for `direction`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DirectionalLightUniform {
    /// See [`DirectionalLight::direction`].
    pub direction: [f32; 3],
    /// Padding only.
    pub _padding0: f32,
    /// See [`DirectionalLight::color`].
    pub color: [f32; 3],
    /// See [`DirectionalLight::intensity`].
    pub intensity: f32,
}

impl DirectionalLightUniform {
    /// All zero (a light contributing nothing) — the "unused slot" filler
    /// in [`LightsUniform::directional_lights`] past `directional_count`.
    pub const ZERO: Self = Self {
        direction: [0.0, 0.0, 0.0],
        _padding0: 0.0,
        color: [0.0, 0.0, 0.0],
        intensity: 0.0,
    };
}

impl From<DirectionalLight> for DirectionalLightUniform {
    fn from(light: DirectionalLight) -> Self {
        Self {
            direction: light.direction.normalize_or_zero().to_array(),
            _padding0: 0.0,
            color: light.color.to_array(),
            intensity: light.intensity,
        }
    }
}

/// GPU-layout [`PointLight`]. `range` doubles as padding to keep the
/// struct 16-byte-aligned, the same trick [`DirectionalLightUniform`]
/// uses `_padding0` for — it just happens to have real data to put there.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PointLightUniform {
    /// See [`PointLight::position`].
    pub position: [f32; 3],
    /// See [`PointLight::range`].
    pub range: f32,
    /// See [`PointLight::color`].
    pub color: [f32; 3],
    /// See [`PointLight::intensity`].
    pub intensity: f32,
    /// Its shadow slot (see [`LightSet::shadowed_lights`]), or `-1`.
    pub shadow_slot: i32,
    /// Padding only: WGSL rounds the struct up to 48 bytes.
    pub _padding: [i32; 3],
}

impl PointLightUniform {
    /// All zero (a light contributing nothing) — the "unused slot" filler
    /// in [`LightsUniform::point_lights`] past `point_count`.
    pub const ZERO: Self = Self {
        position: [0.0, 0.0, 0.0],
        range: 0.0,
        color: [0.0, 0.0, 0.0],
        intensity: 0.0,
        shadow_slot: -1,
        _padding: [0; 3],
    };

    fn with_shadow_slot(self, shadow_slot: i32) -> Self {
        Self {
            shadow_slot,
            ..self
        }
    }
}

impl From<PointLight> for PointLightUniform {
    fn from(light: PointLight) -> Self {
        Self {
            position: light.position.to_array(),
            range: light.range,
            color: light.color.to_array(),
            intensity: light.intensity,
            shadow_slot: -1,
            _padding: [0; 3],
        }
    }
}

/// GPU-layout [`SpotLight`], 64 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpotLightUniform {
    /// See [`SpotLight::position`].
    pub position: [f32; 3],
    /// See [`SpotLight::range`].
    pub range: f32,
    /// See [`SpotLight::direction`], normalized.
    pub direction: [f32; 3],
    /// Cosine of [`SpotLight::outer_angle`].
    pub cos_outer: f32,
    /// See [`SpotLight::color`].
    pub color: [f32; 3],
    /// See [`SpotLight::intensity`].
    pub intensity: f32,
    /// Cosine of [`SpotLight::inner_angle`].
    pub cos_inner: f32,
    /// Its shadow slot (see [`LightSet::shadowed_lights`]), or `-1`.
    pub shadow_slot: i32,
    /// Padding only.
    pub _padding: [f32; 2],
}

impl SpotLightUniform {
    /// All zero (a light contributing nothing).
    pub const ZERO: Self = Self {
        position: [0.0; 3],
        range: 0.0,
        direction: [0.0; 3],
        cos_outer: 1.0,
        color: [0.0; 3],
        intensity: 0.0,
        cos_inner: 1.0,
        shadow_slot: -1,
        _padding: [0.0; 2],
    };

    fn with_shadow_slot(self, shadow_slot: i32) -> Self {
        Self {
            shadow_slot,
            ..self
        }
    }
}

impl From<SpotLight> for SpotLightUniform {
    fn from(light: SpotLight) -> Self {
        let (cos_inner, cos_outer) = light.cone_cosines();
        Self {
            position: light.position.to_array(),
            range: light.range.max(0.0),
            direction: light.direction.normalize_or_zero().to_array(),
            cos_outer,
            color: light.color.to_array(),
            intensity: light.intensity,
            cos_inner,
            shadow_slot: -1,
            _padding: [0.0; 2],
        }
    }
}

/// GPU-layout light set: counts plus fixed-capacity light arrays, ready
/// to write into a uniform buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LightsUniform {
    /// How many of `directional_lights`' entries are real (the rest are
    /// [`DirectionalLightUniform::ZERO`] filler).
    pub directional_count: u32,
    /// How many of `point_lights`' entries are real (the rest are
    /// [`PointLightUniform::ZERO`] filler).
    pub point_count: u32,
    /// Padding only — keeps `directional_lights` starting at a 16-byte
    /// boundary.
    pub _padding: [u32; 2],
    /// Directional lights, `directional_count` real entries followed by
    /// zeroed filler.
    pub directional_lights: [DirectionalLightUniform; MAX_DIRECTIONAL_LIGHTS],
    /// Point lights, `point_count` real entries followed by zeroed
    /// filler.
    pub point_lights: [PointLightUniform; MAX_POINT_LIGHTS],
    /// See [`AmbientLight::sky_color`].
    pub ambient_sky: [f32; 3],
    /// See [`AmbientLight::intensity`]. Packed here to fill the `vec4`
    /// `ambient_sky` would otherwise leave three-quarters used.
    pub ambient_intensity: f32,
    /// See [`AmbientLight::ground_color`].
    pub ambient_ground: [f32; 3],
    /// How many of `spot_lights`' entries are real. Sits where padding
    /// used to, so every field above keeps its offset.
    pub spot_count: u32,
    /// Spot lights, `spot_count` real entries followed by zeroed filler.
    pub spot_lights: [SpotLightUniform; MAX_SPOT_LIGHTS],
}

impl LightsUniform {
    /// No lights at all.
    pub const NONE: Self = Self {
        directional_count: 0,
        point_count: 0,
        _padding: [0; 2],
        directional_lights: [DirectionalLightUniform::ZERO; MAX_DIRECTIONAL_LIGHTS],
        point_lights: [PointLightUniform::ZERO; MAX_POINT_LIGHTS],
        ambient_sky: [0.0; 3],
        ambient_intensity: 0.0,
        ambient_ground: [0.0; 3],
        spot_count: 0,
        spot_lights: [SpotLightUniform::ZERO; MAX_SPOT_LIGHTS],
    };
}

impl Default for LightsUniform {
    /// [`LightsUniform::NONE`].
    fn default() -> Self {
        Self::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_light_set_uploads_zero_counts_but_keeps_ambient() {
        let uniform = LightSet::new().to_uniform();
        assert_eq!(uniform.directional_count, 0);
        assert_eq!(uniform.point_count, 0);
        // Deliberately *not* `LightsUniform::NONE`: since T-20 an empty
        // light set still carries daylight ambient, so a scene with no
        // lights authored yet reads as "outdoors in shade" rather than
        // pure black. `LightsUniform::NONE` remains the genuinely
        // lightless value.
        assert_eq!(uniform.ambient_intensity, AmbientLight::DAYLIGHT.intensity);
        assert_eq!(
            LightSet {
                ambient: AmbientLight::NONE,
                ..LightSet::new()
            }
            .to_uniform(),
            LightsUniform::NONE,
        );
    }

    #[test]
    fn one_directional_light_round_trips() {
        let mut lights = LightSet::new();
        lights.directional.push(DirectionalLight {
            direction: Vec3::new(0.0, -3.0, 0.0), // deliberately non-unit
            color: Vec3::new(1.0, 0.9, 0.8),
            intensity: 2.5,
        });

        let uniform = lights.to_uniform();
        assert_eq!(uniform.directional_count, 1);
        assert_eq!(uniform.directional_lights[0].direction, [0.0, -1.0, 0.0]);
        assert_eq!(uniform.directional_lights[0].color, [1.0, 0.9, 0.8]);
        assert_eq!(uniform.directional_lights[0].intensity, 2.5);
    }

    #[test]
    fn one_point_light_round_trips() {
        let mut lights = LightSet::new();
        lights.point.push(PointLight {
            position: Vec3::new(1.0, 2.0, 3.0),
            color: Vec3::new(0.2, 0.4, 0.6),
            intensity: 5.0,
            range: 10.0,
            casts_shadows: false,
        });

        let uniform = lights.to_uniform();
        assert_eq!(uniform.point_count, 1);
        assert_eq!(uniform.point_lights[0].position, [1.0, 2.0, 3.0]);
        assert_eq!(uniform.point_lights[0].color, [0.2, 0.4, 0.6]);
        assert_eq!(uniform.point_lights[0].intensity, 5.0);
        assert_eq!(uniform.point_lights[0].range, 10.0);
    }

    #[test]
    fn unused_slots_stay_zeroed() {
        let mut lights = LightSet::new();
        lights.directional.push(DirectionalLight {
            direction: Vec3::NEG_Y,
            color: Vec3::ONE,
            intensity: 1.0,
        });
        let uniform = lights.to_uniform();
        assert_eq!(uniform.directional_lights[1], DirectionalLightUniform::ZERO);
        for point_light in uniform.point_lights {
            assert_eq!(point_light, PointLightUniform::ZERO);
        }
    }

    #[test]
    fn excess_lights_beyond_capacity_are_dropped_not_errored() {
        let mut lights = LightSet::new();
        for _ in 0..MAX_DIRECTIONAL_LIGHTS + 3 {
            lights.directional.push(DirectionalLight {
                direction: Vec3::NEG_Y,
                color: Vec3::ONE,
                intensity: 1.0,
            });
        }
        let uniform = lights.to_uniform();
        assert_eq!(uniform.directional_count as usize, MAX_DIRECTIONAL_LIGHTS);
    }

    #[test]
    fn a_busy_view_keeps_the_point_lights_that_matter_most_to_the_camera() {
        let light = |x: f32, intensity: f32| PointLight {
            position: Vec3::new(x, 0.0, 0.0),
            color: Vec3::ONE,
            intensity,
            range: 10.0,
            casts_shadows: false,
        };
        let mut lights = LightSet::new();
        // Far, dim lights first in authored order, then the near ones.
        for index in 0..MAX_POINT_LIGHTS {
            lights.point.push(light(100.0 + index as f32, 1.0));
        }
        let near = light(1.0, 1.0);
        let bright_far = light(50.0, 1_000_000.0);
        lights.point.push(near);
        lights.point.push(bright_far);

        let uniform = lights.to_uniform_for_view(Vec3::ZERO);
        assert_eq!(uniform.point_count as usize, MAX_POINT_LIGHTS);
        let kept: Vec<_> = uniform.point_lights.iter().map(|l| l.position).collect();
        assert!(kept.contains(&near.position.to_array()));
        assert!(kept.contains(&bright_far.position.to_array()));
        assert_eq!(kept[0], bright_far.position.to_array(), "strongest first");

        // Within capacity nothing is reordered or dropped.
        lights.point.truncate(3);
        assert_eq!(lights.to_uniform_for_view(Vec3::ZERO), lights.to_uniform());
    }

    #[test]
    fn shader_point_light_capacity_matches_the_rust_constant() {
        let shader = include_str!("shaders/pbr_shared.wgsl");
        assert!(
            shader.contains(&format!(
                "const MAX_POINT_LIGHTS: u32 = {MAX_POINT_LIGHTS}u;"
            )),
            "pbr_shared.wgsl must declare the same MAX_POINT_LIGHTS"
        );
    }

    #[test]
    fn zero_direction_light_normalizes_to_zero_without_panicking() {
        let uniform = DirectionalLightUniform::from(DirectionalLight {
            direction: Vec3::ZERO,
            color: Vec3::ONE,
            intensity: 1.0,
        });
        assert_eq!(uniform.direction, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn lights_uniform_size_is_a_multiple_of_16_bytes() {
        assert_eq!(size_of::<LightsUniform>() % 16, 0);
    }

    // --- T-20: hemisphere ambient ----------------------------------

    #[test]
    fn hemisphere_ambient_blends_sky_to_ground_by_normal() {
        let ambient = AmbientLight::DAYLIGHT;

        let up = ambient.color_for_normal(Vec3::Y);
        let down = ambient.color_for_normal(Vec3::NEG_Y);
        let side = ambient.color_for_normal(Vec3::X);

        assert!(
            (up - AmbientLight::DAYLIGHT.sky_color).length() < 1e-5,
            "straight up should be the pure sky colour",
        );
        assert!(
            (down - AmbientLight::DAYLIGHT.ground_color).length() < 1e-5,
            "straight down should be the pure ground colour",
        );
        let midpoint =
            (AmbientLight::DAYLIGHT.sky_color + AmbientLight::DAYLIGHT.ground_color) / 2.0;
        assert!(
            (side - midpoint).length() < 1e-5,
            "a horizontal normal should sit halfway between the two",
        );
    }

    #[test]
    fn zero_intensity_ambient_is_fully_dark() {
        let ambient = AmbientLight {
            intensity: 0.0,
            ..AmbientLight::DAYLIGHT
        };
        for normal in [Vec3::Y, Vec3::NEG_Y, Vec3::X, Vec3::Z] {
            assert_eq!(
                ambient.color_for_normal(normal),
                Vec3::ZERO,
                "intensity 0 must remove indirect light entirely",
            );
        }
        assert_eq!(AmbientLight::NONE.color_for_normal(Vec3::Y), Vec3::ZERO);
    }

    #[test]
    fn negative_intensity_is_clamped_rather_than_inverting_light() {
        let ambient = AmbientLight {
            intensity: -5.0,
            ..AmbientLight::DAYLIGHT
        };
        assert_eq!(ambient.color_for_normal(Vec3::Y), Vec3::ZERO);
    }

    #[test]
    fn a_zero_normal_does_not_produce_nan() {
        let color = AmbientLight::DAYLIGHT.color_for_normal(Vec3::ZERO);
        assert!(color.is_finite(), "a degenerate normal must stay finite");
    }

    #[test]
    fn ambient_round_trips_through_the_lights_uniform() {
        let mut lights = LightSet::new();
        lights.ambient = AmbientLight {
            sky_color: Vec3::new(0.1, 0.2, 0.3),
            ground_color: Vec3::new(0.4, 0.5, 0.6),
            intensity: 0.75,
        };

        let uniform = lights.to_uniform();

        assert_eq!(uniform.ambient_sky, [0.1, 0.2, 0.3]);
        assert_eq!(uniform.ambient_ground, [0.4, 0.5, 0.6]);
        assert!((uniform.ambient_intensity - 0.75).abs() < 1e-6);
    }

    #[test]
    fn uniform_negative_intensity_is_clamped_on_upload() {
        let mut lights = LightSet::new();
        lights.ambient = AmbientLight {
            intensity: -1.0,
            ..AmbientLight::DAYLIGHT
        };
        assert_eq!(lights.to_uniform().ambient_intensity, 0.0);
    }

    #[test]
    fn default_light_set_is_lit_by_daylight_ambient() {
        assert_eq!(LightSet::new().ambient, AmbientLight::DAYLIGHT);
        assert!(
            AmbientLight::DAYLIGHT.color_for_normal(Vec3::Y).length() > 0.03,
            "the default must be meaningfully brighter than the flat 0.03 it replaced",
        );
    }

    #[test]
    fn lights_uniform_is_16_byte_aligned() {
        assert_eq!(
            std::mem::size_of::<LightsUniform>() % 16,
            0,
            "a WGSL uniform struct must be a multiple of 16 bytes",
        );
    }

    fn spot(x: f32, casts_shadows: bool) -> SpotLight {
        SpotLight {
            position: Vec3::new(x, 0.0, 0.0),
            direction: Vec3::NEG_Y,
            color: Vec3::ONE,
            intensity: 10.0,
            range: 10.0,
            inner_angle: 20f32.to_radians(),
            outer_angle: 30f32.to_radians(),
            casts_shadows,
        }
    }

    #[test]
    fn spot_light_is_bright_in_the_cone_and_dark_outside_and_past_its_range() {
        let light = spot(0.0, false);
        assert!(light.cone_and_range_at(Vec3::new(0.0, -2.0, 0.0)) > 0.9);
        // 45° off the axis, beyond the 30° outer cone.
        assert_eq!(light.cone_and_range_at(Vec3::new(2.0, -2.0, 0.0)), 0.0);
        // Between inner and outer: partly lit.
        let between = light.cone_and_range_at(Vec3::new(25f32.to_radians().tan() * 2.0, -2.0, 0.0));
        assert!(between > 0.0 && between < 1.0, "{between}");
        assert_eq!(light.cone_and_range_at(Vec3::new(0.0, -10.5, 0.0)), 0.0);
    }

    #[test]
    fn spot_lights_pack_with_their_cone() {
        let mut lights = LightSet::new();
        lights.spot.push(spot(1.0, false));
        let uniform = lights.to_uniform();
        assert_eq!(uniform.spot_count, 1);
        let packed = uniform.spot_lights[0];
        assert_eq!(packed.position, [1.0, 0.0, 0.0]);
        assert_eq!(packed.direction, [0.0, -1.0, 0.0]);
        assert!((packed.cos_inner - 20f32.to_radians().cos()).abs() < 1e-5);
        assert!(packed.cos_outer < packed.cos_inner);
        assert_eq!(packed.shadow_slot, -1);
    }

    #[test]
    fn the_nearest_shadow_casting_lights_get_the_shadow_slots() {
        let point = |x: f32, casts_shadows: bool| PointLight {
            position: Vec3::new(x, 0.0, 0.0),
            color: Vec3::ONE,
            intensity: 10.0,
            range: 10.0,
            casts_shadows,
        };
        let mut lights = LightSet::new();
        lights.point.push(point(1.0, false));
        for x in [50.0, 2.0, 40.0, 3.0] {
            lights.point.push(point(x, true));
        }
        lights.spot.push(spot(4.0, true));
        lights.spot.push(spot(500.0, true));
        let shadowed = lights.shadowed_lights(Vec3::ZERO);
        assert_eq!(
            shadowed,
            vec![
                ShadowedLight::Point(2),
                ShadowedLight::Point(4),
                ShadowedLight::Spot(0),
                ShadowedLight::Point(3),
            ]
        );
        let uniform = lights.to_uniform_for_view(Vec3::ZERO);
        assert_eq!(
            uniform.point_lights[0].shadow_slot, -1,
            "not a shadow caster"
        );
        assert_eq!(uniform.point_lights[1].shadow_slot, -1, "too far");
        assert_eq!(uniform.point_lights[2].shadow_slot, 0);
        assert_eq!(uniform.point_lights[4].shadow_slot, 1);
        assert_eq!(uniform.spot_lights[0].shadow_slot, 2);
        assert_eq!(uniform.spot_lights[1].shadow_slot, -1);
    }

    #[test]
    fn shader_light_capacities_match_the_rust_constants() {
        let shader = include_str!("shaders/pbr_shared.wgsl");
        for (name, value) in [
            ("MAX_SPOT_LIGHTS", MAX_SPOT_LIGHTS),
            ("MAX_SHADOWED_LIGHTS", MAX_SHADOWED_LIGHTS),
        ] {
            assert!(
                shader.contains(&format!("const {name}: u32 = {value}u;")),
                "pbr_shared.wgsl must declare the same {name}"
            );
        }
        assert_eq!(size_of::<PointLightUniform>(), 48);
        assert_eq!(size_of::<SpotLightUniform>(), 64);
    }
}
