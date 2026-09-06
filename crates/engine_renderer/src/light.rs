//! Directional and point lights, and their GPU-uniform representation.
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
/// How many point lights [`LightSet::to_uniform`] uploads. Extra lights
/// beyond this are silently dropped — see [`LightSet::to_uniform`].
pub const MAX_POINT_LIGHTS: usize = 4;

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
    /// getting very dim.
    pub range: f32,
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
/// The blend is mirrored in `pbr_common.wgsl`; see
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
    /// This is the CPU mirror of the WGSL in `pbr_common.wgsl`. Keep both
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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LightSet {
    /// Directional lights (e.g. one sun).
    pub directional: Vec<DirectionalLight>,
    /// Point lights.
    pub point: Vec<PointLight>,
    /// Indirect light. Defaults to [`AmbientLight::DAYLIGHT`].
    pub ambient: AmbientLight,
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
    pub fn to_uniform(&self) -> LightsUniform {
        let mut directional_lights = [DirectionalLightUniform::ZERO; MAX_DIRECTIONAL_LIGHTS];
        for (slot, light) in directional_lights.iter_mut().zip(&self.directional) {
            *slot = DirectionalLightUniform::from(*light);
        }

        let mut point_lights = [PointLightUniform::ZERO; MAX_POINT_LIGHTS];
        for (slot, light) in point_lights.iter_mut().zip(&self.point) {
            *slot = PointLightUniform::from(*light);
        }

        LightsUniform {
            directional_count: self.directional.len().min(MAX_DIRECTIONAL_LIGHTS) as u32,
            point_count: self.point.len().min(MAX_POINT_LIGHTS) as u32,
            _padding: [0; 2],
            directional_lights,
            point_lights,
            ambient_sky: self.ambient.sky_color.to_array(),
            ambient_intensity: self.ambient.intensity.max(0.0),
            ambient_ground: self.ambient.ground_color.to_array(),
            _padding1: 0.0,
        }
    }
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
}

impl PointLightUniform {
    /// All zero (a light contributing nothing) — the "unused slot" filler
    /// in [`LightsUniform::point_lights`] past `point_count`.
    pub const ZERO: Self = Self {
        position: [0.0, 0.0, 0.0],
        range: 0.0,
        color: [0.0, 0.0, 0.0],
        intensity: 0.0,
    };
}

impl From<PointLight> for PointLightUniform {
    fn from(light: PointLight) -> Self {
        Self {
            position: light.position.to_array(),
            range: light.range,
            color: light.color.to_array(),
            intensity: light.intensity,
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
    /// Padding only — keeps the struct a multiple of 16 bytes.
    pub _padding1: f32,
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
        _padding1: 0.0,
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
}
