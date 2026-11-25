//! The sky and the air: fog, clouds, sky colours, time of day and the
//! optional HDRI sky image. Pure data and math, so it can be tested without
//! a GPU; [`crate::skybox_uniform`] and [`crate::Camera::to_uniform_with_fog`]
//! carry it to the shaders.

use glam::Vec3;

use crate::AmbientLight;

/// Fog: distance fog that thickens past `start`, plus height fog that is
/// thickest low down and thins out above `height`. All zero is no fog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fog {
    /// Linear RGB colour of the fog.
    pub color: Vec3,
    /// How quickly distance fog thickens, per world unit.
    pub density: f32,
    /// Distance from the camera where distance fog begins.
    pub start: f32,
    /// How thick height fog is at `height`, per world unit.
    pub height_density: f32,
    /// World-space height where height fog has `height_density`.
    pub height: f32,
    /// How quickly height fog thins out going up, per world unit.
    pub height_falloff: f32,
}

impl Fog {
    /// No fog at all.
    pub const NONE: Self = Self {
        color: Vec3::ZERO,
        density: 0.0,
        start: 0.0,
        height_density: 0.0,
        height: 0.0,
        height_falloff: 1.0,
    };

    /// Whether this fog does anything.
    pub fn is_on(&self) -> bool {
        self.density > 0.0 || self.height_density > 0.0
    }

    /// How much of the colour at `point` is fog for a camera at `eye`, in
    /// `[0, 1]`. The CPU mirror of `fog_amount` in `pbr_shared.wgsl` (and
    /// its copy in `water.wgsl`); keep them in step.
    pub fn amount(&self, eye: Vec3, point: Vec3) -> f32 {
        let distance = eye.distance(point);
        let past_start = (distance - self.start).max(0.0);
        let mut optical = self.density.max(0.0) * past_start;
        if self.height_density > 0.0 && distance > 0.0 {
            let falloff = self.height_falloff.max(1e-4);
            // Height fog integrated along the ray: density
            // `height_density * exp(-falloff * (y - height))`.
            let start_density = (-falloff * (eye.y - self.height)).clamp(-20.0, 20.0).exp();
            let rise = falloff * (point.y - eye.y);
            let spread = if rise.abs() < 1e-4 {
                1.0
            } else {
                (1.0 - (-rise).clamp(-20.0, 20.0).exp()) / rise
            };
            optical += self.height_density * start_density * spread * past_start;
        }
        (1.0 - (-optical).exp()).clamp(0.0, 1.0)
    }
}

impl Default for Fog {
    fn default() -> Self {
        Self::NONE
    }
}

/// A layer of clouds drifting across the sky.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clouds {
    /// How much of the sky is covered, `0` (clear) to `1` (overcast).
    pub coverage: f32,
    /// How fast they drift.
    pub speed: f32,
    /// Linear RGB colour of their lit side.
    pub color: Vec3,
}

/// The procedural sky's colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyColors {
    /// Straight up.
    pub zenith: Vec3,
    /// At the horizon.
    pub horizon: Vec3,
    /// Below the horizon, mixed in by `ground_weight` (0 keeps the sky
    /// gradient going down).
    pub ground: Vec3,
    /// How much `ground` shows below the horizon, `0` to `1`.
    pub ground_weight: f32,
}

impl SkyColors {
    /// The sky Studio has always drawn: a blue gradient, no ground colour.
    pub const DEFAULT: Self = Self {
        zenith: Vec3::new(0.10, 0.25, 0.55),
        horizon: Vec3::new(0.65, 0.75, 0.85),
        ground: Vec3::new(0.30, 0.26, 0.20),
        ground_weight: 0.0,
    };
}

impl Default for SkyColors {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The sky and air of a frame, handed to rendering with the lights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Atmosphere {
    /// Sky colours (ignored behind a sky image).
    pub sky: SkyColors,
    /// The cloud layer, if any.
    pub clouds: Option<Clouds>,
    /// Fog.
    pub fog: Fog,
    /// Seconds of cloud drift; kept small (it wraps) so the noise stays
    /// precise.
    pub time: f32,
    /// Whether the sky shows the sky image set on the skybox binding.
    pub image: bool,
}

impl Atmosphere {
    /// How long cloud time runs before wrapping, in seconds.
    pub const TIME_WRAP: f32 = 3600.0;
}

impl Default for Atmosphere {
    /// Today's sky: the default gradient, no clouds, no fog.
    fn default() -> Self {
        Self {
            sky: SkyColors::DEFAULT,
            clouds: None,
            fog: Fog::NONE,
            time: 0.0,
            image: false,
        }
    }
}

/// The sun and sky at one time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayLight {
    /// The way sunlight travels (see [`crate::DirectionalLight::direction`]).
    pub sun_direction: Vec3,
    /// Sunlight colour.
    pub sun_color: Vec3,
    /// Multiplies the sun's authored strength: 1 at full day, 0 at night.
    pub sun_strength: f32,
    /// Sky colours.
    pub sky: SkyColors,
    /// Ambient sky and ground colours (intensity 1).
    pub ambient: AmbientLight,
}

impl DayLight {
    /// Lights `lights` at this time: the first sun takes this direction and
    /// colour, its authored strength scaled by [`Self::sun_strength`], and
    /// the ambient light takes these colours at its authored intensity.
    pub fn apply(&self, lights: &mut crate::LightSet) {
        if let Some(sun) = lights.directional.first_mut() {
            sun.direction = self.sun_direction;
            sun.color = self.sun_color;
            sun.intensity *= self.sun_strength;
        }
        lights.ambient.sky_color = self.ambient.sky_color;
        lights.ambient.ground_color = self.ambient.ground_color;
    }
}

/// How high the sun climbs at noon, in degrees.
const NOON_ELEVATION_DEGREES: f32 = 60.0;

/// The sun, sky and ambient light at `hour` (0–24, wrapping): the sun rises
/// in the east (+X) at 6:00, is highest at noon and sets in the west at
/// 18:00; between 18:00 and 6:00 it is below the horizon and it is night.
pub fn time_of_day(hour: f32) -> DayLight {
    let hour = hour.rem_euclid(24.0);
    let angle = (hour - 6.0) / 12.0 * std::f32::consts::PI;
    let tilt = NOON_ELEVATION_DEGREES.to_radians();
    let to_sun = Vec3::new(
        angle.cos(),
        angle.sin() * tilt.sin(),
        -angle.sin() * tilt.cos(),
    )
    .normalize_or(Vec3::Y);
    let height = to_sun.y;

    let day = smoothstep(-0.15, 0.05, height);
    let noon = smoothstep(0.0, 0.4, height);
    let lerp = |a: Vec3, b: Vec3, t: f32| a.lerp(b, t);

    let sunset_sun = Vec3::new(1.0, 0.45, 0.2);
    let noon_sun = Vec3::new(1.0, 0.98, 0.92);
    let sun_color = lerp(sunset_sun, noon_sun, noon);

    let sunset_sky = SkyColors {
        zenith: Vec3::new(0.20, 0.24, 0.45),
        horizon: Vec3::new(0.95, 0.55, 0.35),
        ground: Vec3::new(0.18, 0.14, 0.12),
        ground_weight: 0.6,
    };
    let noon_sky = SkyColors {
        ground_weight: 0.6,
        ..SkyColors::DEFAULT
    };
    let night_sky = SkyColors {
        zenith: Vec3::new(0.006, 0.01, 0.03),
        horizon: Vec3::new(0.025, 0.04, 0.08),
        ground: Vec3::new(0.01, 0.01, 0.015),
        ground_weight: 0.6,
    };
    let day_sky = SkyColors {
        zenith: lerp(sunset_sky.zenith, noon_sky.zenith, noon),
        horizon: lerp(sunset_sky.horizon, noon_sky.horizon, noon),
        ground: lerp(sunset_sky.ground, noon_sky.ground, noon),
        ground_weight: 0.6,
    };
    let sky = SkyColors {
        zenith: lerp(night_sky.zenith, day_sky.zenith, day),
        horizon: lerp(night_sky.horizon, day_sky.horizon, day),
        ground: lerp(night_sky.ground, day_sky.ground, day),
        ground_weight: 0.6,
    };

    let night_ambient = (Vec3::new(0.03, 0.045, 0.09), Vec3::new(0.02, 0.02, 0.03));
    let sunset_ambient = (Vec3::new(0.45, 0.38, 0.45), Vec3::new(0.30, 0.20, 0.15));
    let noon_ambient = (
        AmbientLight::DAYLIGHT.sky_color,
        AmbientLight::DAYLIGHT.ground_color,
    );
    let day_ambient = (
        lerp(sunset_ambient.0, noon_ambient.0, noon),
        lerp(sunset_ambient.1, noon_ambient.1, noon),
    );
    let ambient = AmbientLight {
        sky_color: lerp(night_ambient.0, day_ambient.0, day),
        ground_color: lerp(night_ambient.1, day_ambient.1, day),
        intensity: 1.0,
    };

    DayLight {
        sun_direction: -to_sun,
        sun_color,
        sun_strength: smoothstep(-0.02, 0.1, height),
        sky,
        ambient,
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The widest sky image kept, in pixels; larger ones are scaled down.
pub const MAX_SKY_IMAGE_WIDTH: u32 = 4096;

/// A decoded sky image: an equirectangular panorama in half-float RGBA,
/// ready to upload, and the ambient light it gives.
#[derive(Debug, Clone, PartialEq)]
pub struct SkyImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major RGBA, IEEE half-float bits.
    pub rgba16f: Vec<u16>,
    /// The mean colour of the upper half (sky) and lower half (ground),
    /// as ambient light.
    pub ambient: AmbientLight,
}

/// Decodes a sky image (`.hdr`, `.exr`, `.png` or `.jpg`), keeping HDR
/// brightness, scaled down to at most [`MAX_SKY_IMAGE_WIDTH`] wide.
///
/// # Errors
///
/// A message when the bytes aren't an image `image` can read.
pub fn decode_sky_image(bytes: &[u8]) -> Result<SkyImage, String> {
    let decoded = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let decoded = if decoded.width() > MAX_SKY_IMAGE_WIDTH {
        let height = (u64::from(decoded.height()) * u64::from(MAX_SKY_IMAGE_WIDTH)
            / u64::from(decoded.width().max(1))) as u32;
        decoded.resize_exact(
            MAX_SKY_IMAGE_WIDTH,
            height.max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let pixels = decoded.to_rgba32f();
    let (width, height) = pixels.dimensions();
    let mut sums = [Vec3::ZERO; 2];
    let mut counts = [0u32; 2];
    let mut rgba16f = Vec::with_capacity(pixels.len());
    for (row, line) in pixels.rows().enumerate() {
        let half = usize::from(row as u32 >= height / 2);
        for pixel in line {
            let [r, g, b, a] = pixel.0;
            let color = Vec3::new(r, g, b).max(Vec3::ZERO);
            if color.is_finite() {
                sums[half] += color;
                counts[half] += 1;
            }
            rgba16f.extend([r, g, b, a].map(f32_to_f16));
        }
    }
    let mean = |half: usize| sums[half] / counts[half].max(1) as f32;
    Ok(SkyImage {
        width,
        height,
        rgba16f,
        ambient: AmbientLight {
            sky_color: mean(0),
            ground_color: mean(1),
            intensity: 1.0,
        },
    })
}

/// IEEE half-float bits for `value`, rounded to nearest; out-of-range
/// values become infinity, NaN stays NaN.
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;
    if exponent == 0xff {
        let nan = if mantissa != 0 { 0x0200 } else { 0 };
        return sign | 0x7c00 | nan;
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 0x1f {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = mantissa | 0x0080_0000;
        let shift = (14 - half_exponent) as u32;
        let rounded = (mantissa + (1 << (shift - 1))) >> shift;
        return sign | rounded as u16;
    }
    let rounded = mantissa + 0x1000;
    if rounded & 0x0080_0000 != 0 {
        // The mantissa overflowed into the exponent.
        let half_exponent = half_exponent + 1;
        if half_exponent >= 0x1f {
            return sign | 0x7c00;
        }
        return sign | ((half_exponent as u16) << 10);
    }
    sign | ((half_exponent as u16) << 10) | ((rounded >> 13) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fog_hides_nothing_and_fog_thickens_with_distance() {
        let eye = Vec3::new(0.0, 2.0, 0.0);
        let far = Vec3::new(0.0, 2.0, -200.0);
        assert_eq!(Fog::NONE.amount(eye, far), 0.0);
        let fog = Fog {
            color: Vec3::ONE,
            density: 0.02,
            start: 10.0,
            ..Fog::NONE
        };
        assert_eq!(
            fog.amount(eye, Vec3::new(0.0, 2.0, -5.0)),
            0.0,
            "before start"
        );
        let near = fog.amount(eye, Vec3::new(0.0, 2.0, -30.0));
        let farther = fog.amount(eye, far);
        assert!(
            near > 0.0 && near < farther && farther < 1.0,
            "{near} {farther}"
        );
    }

    #[test]
    fn height_fog_is_thicker_low_down() {
        let fog = Fog {
            height_density: 0.05,
            height: 0.0,
            height_falloff: 0.3,
            ..Fog::NONE
        };
        let eye = Vec3::new(0.0, 1.0, 0.0);
        let low = fog.amount(eye, Vec3::new(0.0, 0.0, -50.0));
        let high = fog.amount(Vec3::new(0.0, 30.0, 0.0), Vec3::new(0.0, 30.0, -50.0));
        assert!(low > high * 2.0, "{low} {high}");
        // A level ray and one that rises a little agree near the limit.
        let level = fog.amount(eye, Vec3::new(0.0, 1.0, -50.0));
        let rising = fog.amount(eye, Vec3::new(0.0, 1.0001, -50.0));
        assert!((level - rising).abs() < 1e-3);
    }

    #[test]
    fn the_sun_rises_in_the_east_peaks_at_noon_and_sets_at_night() {
        let dawn = time_of_day(6.0);
        assert!((-dawn.sun_direction).x > 0.99, "rises in the east (+X)");
        let noon = time_of_day(12.0);
        let height = (-noon.sun_direction).y;
        assert!((height - NOON_ELEVATION_DEGREES.to_radians().sin()).abs() < 1e-4);
        assert_eq!(noon.sun_strength, 1.0);
        let midnight = time_of_day(0.0);
        assert!((-midnight.sun_direction).y < 0.0);
        assert_eq!(midnight.sun_strength, 0.0);
        assert!(midnight.ambient.sky_color.length() < noon.ambient.sky_color.length());
        // No jump across midnight.
        let before = time_of_day(23.999);
        let after = time_of_day(24.001);
        assert!((before.sun_direction - after.sun_direction).length() < 1e-3);
        assert_eq!(time_of_day(25.0), time_of_day(1.0));
    }

    #[test]
    fn the_default_sky_keeps_the_old_gradient() {
        assert_eq!(SkyColors::DEFAULT.zenith, Vec3::new(0.10, 0.25, 0.55));
        assert_eq!(SkyColors::DEFAULT.horizon, Vec3::new(0.65, 0.75, 0.85));
        assert_eq!(SkyColors::DEFAULT.ground_weight, 0.0);
        assert_eq!(Atmosphere::default().fog, Fog::NONE);
    }

    #[test]
    fn half_floats_round_trip_common_values() {
        let back = |bits: u16| {
            let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
            let exponent = i32::from((bits >> 10) & 0x1f);
            let mantissa = f32::from(bits & 0x3ff);
            if exponent == 0 {
                return sign * mantissa * 2f32.powi(-24);
            }
            sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15)
        };
        for value in [0.0, 1.0, -2.5, 0.5, 1000.0, 0.0001, 65504.0] {
            let converted = back(f32_to_f16(value));
            assert!(
                (converted - value).abs() <= value.abs() * 1e-3 + 1e-7,
                "{value} -> {converted}"
            );
        }
        assert_eq!(f32_to_f16(1.0e6), 0x7c00, "too big is infinity");
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7c00);
        assert!(f32_to_f16(f32::NAN) & 0x03ff != 0, "NaN stays NaN");
    }

    #[test]
    fn a_sky_image_keeps_hdr_brightness_and_gives_ambient_colours() {
        // 4 x 2: a bright blue sky row over a dark brown ground row.
        let mut image = image::Rgb32FImage::new(4, 2);
        for x in 0..4 {
            image.put_pixel(x, 0, image::Rgb([0.5, 1.0, 4.0]));
            image.put_pixel(x, 1, image::Rgb([0.2, 0.1, 0.05]));
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb32F(image)
            .write_to(&mut bytes, image::ImageFormat::Hdr)
            .expect("encode .hdr");
        let sky = decode_sky_image(bytes.get_ref()).expect("decode");
        assert_eq!((sky.width, sky.height), (4, 2));
        assert_eq!(sky.rgba16f.len(), 4 * 2 * 4);
        assert!(sky.ambient.sky_color.z > 3.0, "{:?}", sky.ambient);
        assert!(sky.ambient.ground_color.x < 0.3, "{:?}", sky.ambient);
        assert!(decode_sky_image(b"not an image").is_err());
    }
}
