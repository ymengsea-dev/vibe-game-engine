//! HDR intermediate render target and tonemap-uniform GPU-layout struct.
//!
//! The main color pass renders into an off-screen floating-point texture
//! (see [`crate::HdrTarget`]) rather than directly into the LDR swapchain,
//! so lighting math that produces radiance above `1.0` (bright specular
//! highlights, the skybox's sun glow, ...) doesn't hard-clip. A separate
//! full-screen tonemap pass (`tonemap.wgsl`, [`crate::TonemapPipeline`])
//! then compresses that into the swapchain's displayable range with a
//! filmic curve instead of a harsh clip.

/// Multiplies HDR radiance before the tonemap curve — a scene-wide
/// brightness knob. `1.0` leaves radiance as computed by the lighting
/// pipeline.
pub const DEFAULT_EXPOSURE: f32 = 1.0;

/// GPU-layout tonemap pass input, ready to write into a uniform buffer.
///
/// Same `#[repr(C)]` + `Pod`/`Zeroable` pattern as [`crate::CameraUniform`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TonemapUniform {
    /// See [`DEFAULT_EXPOSURE`].
    pub exposure: f32,
    /// Padding only — keeps the struct a multiple of 16 bytes (the WGSL
    /// uniform-address-space struct-size alignment rule).
    pub _padding: [f32; 3],
}

impl TonemapUniform {
    /// [`DEFAULT_EXPOSURE`], packed for GPU upload.
    pub const DEFAULT: Self = Self {
        exposure: DEFAULT_EXPOSURE,
        _padding: [0.0; 3],
    };
}

impl Default for TonemapUniform {
    /// [`TonemapUniform::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<f32> for TonemapUniform {
    /// Packs an exposure value for GPU upload.
    fn from(exposure: f32) -> Self {
        Self {
            exposure,
            _padding: [0.0; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_default_exposure() {
        assert_eq!(TonemapUniform::default().exposure, DEFAULT_EXPOSURE);
    }

    #[test]
    fn default_matches_default_const() {
        assert_eq!(TonemapUniform::default(), TonemapUniform::DEFAULT);
    }

    #[test]
    fn from_f32_round_trips_exposure() {
        assert_eq!(TonemapUniform::from(2.5).exposure, 2.5);
    }

    #[test]
    fn uniform_bytes_are_16_bytes() {
        assert_eq!(bytemuck::bytes_of(&TonemapUniform::DEFAULT).len(), 16);
    }

    #[test]
    fn uniform_size_is_a_multiple_of_16_bytes() {
        assert_eq!(size_of::<TonemapUniform>() % 16, 0);
    }
}
