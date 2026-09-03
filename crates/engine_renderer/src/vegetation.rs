//! Wind-animated, GPU-instanced vegetation: a second instanced PBR
//! pipeline whose vertex stage bends each blade along a global wind
//! direction, pivoting at the plant's root (object-space `y = 0`).
//!
//! Reuses the instancing path wholesale — [`crate::InstanceRaw`] /
//! [`crate::InstancedDrawable`], the same instance buffer, the same
//! camera/material/lights bind groups. The only additions are
//! [`VegetationPipeline`] (`pbr_common.wgsl` + `vegetation_vs.wgsl`) and a
//! [`WindBinding`] at the `@group(2)` slot the plain instanced path leaves
//! unbound. [`GpuContext::render_scene`] takes an optional
//! `(&VegetationPipeline, &WindBinding, &[InstancedDrawable])` and draws it
//! in the scene pass, into the HDR target, so bloom/tonemap apply.
//!
//! Deliberately minimal for one iteration: one global wind (per-plant
//! variation comes from hashing the instance's world origin into the sway
//! phase in-shader), opaque double-sided blades (no alpha-tested foliage
//! textures), and no normal correction for the bend. LOD/density fade,
//! textured blades, flowmap wind, and grass-vs-terrain-normal alignment
//! are all future work.

use glam::Vec2;

use crate::gpu::GpuContext;
use crate::mesh::Vertex;
use crate::pipeline::{PBR_COMMON_SOURCE, Pipeline};

const VEGETATION_VS_SOURCE: &str = include_str!("shaders/vegetation_vs.wgsl");

/// Global wind parameters for the vegetation pipeline. `strength` is the
/// sway displacement (world units) at a vertex one unit above the plant's
/// base, at full oscillation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wind {
    /// Heading on the ground plane, `(x, z)`. Normalized on upload; a zero
    /// vector falls back to `+X`.
    pub direction: Vec2,
    /// Sway displacement per unit height at full oscillation. Negative
    /// values are clamped to `0.0`.
    pub strength: f32,
    /// Primary oscillation rate, radians per second. Clamped to `>= 0.0`.
    pub frequency: f32,
    /// Secondary high-frequency flutter, as a fraction of the primary
    /// amplitude. Clamped to `>= 0.0`.
    pub turbulence: f32,
}

impl Wind {
    /// Still air — no sway.
    pub const CALM: Self = Self {
        direction: Vec2::X,
        strength: 0.0,
        frequency: 0.0,
        turbulence: 0.0,
    };
    /// A light, lively breeze — a sensible default for a grass field.
    pub const BREEZE: Self = Self {
        direction: Vec2::new(1.0, 0.35),
        strength: 0.12,
        frequency: 2.2,
        turbulence: 0.4,
    };

    /// Packs this wind (and the current animation `time`, in seconds) for
    /// GPU upload: direction normalized (`+X` if degenerate), the scalar
    /// parameters clamped non-negative.
    pub fn to_uniform(self, time: f32) -> WindUniform {
        let direction = {
            let normalized = self.direction.normalize_or_zero();
            if normalized == Vec2::ZERO {
                Vec2::X
            } else {
                normalized
            }
        };
        WindUniform {
            time,
            strength: self.strength.max(0.0),
            frequency: self.frequency.max(0.0),
            turbulence: self.turbulence.max(0.0),
            direction: direction.to_array(),
            _padding: [0.0; 2],
        }
    }
}

impl Default for Wind {
    /// [`Wind::BREEZE`].
    fn default() -> Self {
        Self::BREEZE
    }
}

/// GPU-layout wind uniform for [`VegetationPipeline`]'s `@group(2)`. Same
/// `#[repr(C)]` + `Pod`/`Zeroable` pattern as [`crate::CameraUniform`];
/// 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WindUniform {
    /// Animation time, seconds. The caller keeps this bounded (e.g.
    /// wrapped modulo a large value) so `sin` stays precise.
    pub time: f32,
    /// See [`Wind::strength`] (clamped `>= 0`).
    pub strength: f32,
    /// See [`Wind::frequency`] (clamped `>= 0`).
    pub frequency: f32,
    /// See [`Wind::turbulence`] (clamped `>= 0`).
    pub turbulence: f32,
    /// Normalized wind heading `(x, z)`.
    pub direction: [f32; 2],
    /// Padding to a 16-byte multiple.
    pub _padding: [f32; 2],
}

/// A compiled wind-animated instanced PBR pipeline. Its shader is
/// `pbr_common.wgsl` + `vegetation_vs.wgsl` — identical lighting/fragment
/// stage to [`Pipeline`], a wind-bending instanced vertex stage.
pub struct VegetationPipeline {
    pub(crate) render_pipeline: wgpu::RenderPipeline,
    wind_bind_group_layout: wgpu::BindGroupLayout,
}

/// A [`WindUniform`] buffer plus the bind group exposing it to
/// [`VegetationPipeline`]'s shader at `@group(2)`. Rewrite the buffer each
/// frame via [`GpuContext::write_wind`].
pub struct WindBinding {
    /// The GPU buffer backing the uniform. Kept for per-frame updates.
    pub buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
}

impl GpuContext {
    /// Compiles the vegetation shader into a [`VegetationPipeline`],
    /// reusing `base`'s camera (`@group(0)`), material (`@group(1)`), and
    /// lights (`@group(3)`) bind group layouts and adding a wind layout at
    /// `@group(2)`.
    pub fn create_vegetation_pipeline(&self, base: &Pipeline, label: &str) -> VegetationPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(
                format!("{PBR_COMMON_SOURCE}\n{VEGETATION_VS_SOURCE}").into(),
            ),
        });

        let wind_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wind bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[
                Some(base.camera_layout()),
                Some(base.material_layout()),
                Some(&wind_bind_group_layout),
                Some(base.lights_layout()),
            ],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(Vertex::layout()), Some(crate::InstanceRaw::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(crate::pipeline::HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                // Blades are thin double-sided geometry — no culling.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        VegetationPipeline {
            render_pipeline,
            wind_bind_group_layout,
        }
    }

    /// Creates a [`WindBinding`]: a [`WindUniform`] buffer seeded from
    /// `wind` at `time`, plus the bind group exposing it to `pipeline`'s
    /// shader.
    pub fn create_wind_binding(
        &self,
        pipeline: &VegetationPipeline,
        wind: &Wind,
        time: f32,
    ) -> WindBinding {
        let buffer = self.create_uniform_buffer("wind uniform", &wind.to_uniform(time));
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wind bind group"),
            layout: &pipeline.wind_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        WindBinding { buffer, bind_group }
    }

    /// Rewrites `binding`'s uniform buffer from `wind` at `time`. Call once
    /// per frame, before [`GpuContext::render_scene`].
    pub fn write_wind(&self, binding: &WindBinding, wind: &Wind, time: f32) {
        self.write_uniform_buffer(&binding.buffer, &wind.to_uniform(time));
    }
}

/// CPU mirror of `vegetation_vs.wgsl`'s `wind_sway` — the sway offset
/// (world units, along the wind direction) for a vertex at object-space
/// height `local_y` on a plant whose world origin yields `phase`. Kept in
/// sync with the shader and exercised by the tests below.
#[cfg(test)]
pub(crate) fn wind_sway(local_y: f32, phase: f32, uniform: &WindUniform) -> f32 {
    let bend = local_y.max(0.0);
    let primary = (uniform.time * uniform.frequency + phase).sin();
    let flutter = (uniform.time * uniform.frequency * 3.7 + phase * 2.3).sin() * uniform.turbulence;
    (primary + flutter) * uniform.strength * bend
}

/// The per-plant sway phase `vegetation_vs.wgsl` derives from an
/// instance's world origin.
#[cfg(test)]
pub(crate) fn plant_phase(origin_x: f32, origin_z: f32) -> f32 {
    origin_x * 0.7 + origin_z * 0.7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wind_uniform_is_32_bytes_and_16_aligned() {
        assert_eq!(size_of::<WindUniform>(), 32);
        assert_eq!(size_of::<WindUniform>() % 16, 0);
    }

    #[test]
    fn wind_uniform_round_trips_through_bytes() {
        let uniform = Wind::BREEZE.to_uniform(3.5);
        let bytes = bytemuck::bytes_of(&uniform);
        let back: WindUniform = *bytemuck::from_bytes(bytes);
        assert_eq!(uniform, back);
    }

    #[test]
    fn to_uniform_normalizes_direction_and_carries_time() {
        let wind = Wind {
            direction: Vec2::new(3.0, 4.0),
            strength: 0.2,
            frequency: 1.5,
            turbulence: 0.3,
        };
        let uniform = wind.to_uniform(7.0);
        let dir = Vec2::from_array(uniform.direction);
        assert!((dir.length() - 1.0).abs() < 1e-6);
        assert_eq!(uniform.time, 7.0);
    }

    #[test]
    fn to_uniform_falls_back_to_plus_x_for_a_zero_direction() {
        let uniform = Wind {
            direction: Vec2::ZERO,
            strength: 0.1,
            frequency: 1.0,
            turbulence: 0.0,
        }
        .to_uniform(0.0);
        assert_eq!(uniform.direction, [1.0, 0.0]);
    }

    #[test]
    fn to_uniform_clamps_negative_scalars_to_zero() {
        let uniform = Wind {
            direction: Vec2::X,
            strength: -1.0,
            frequency: -2.0,
            turbulence: -0.5,
        }
        .to_uniform(0.0);
        assert_eq!(uniform.strength, 0.0);
        assert_eq!(uniform.frequency, 0.0);
        assert_eq!(uniform.turbulence, 0.0);
    }

    #[test]
    fn breeze_is_windier_than_calm() {
        // Via `to_uniform` (not a const fn) so the comparison isn't a
        // compile-time constant.
        let calm = Wind::CALM.to_uniform(1.0);
        let breeze = Wind::BREEZE.to_uniform(1.0);
        assert_eq!(calm.strength, 0.0);
        assert!(breeze.strength > calm.strength);
    }

    #[test]
    fn sway_pins_the_root_and_grows_with_height() {
        let uniform = Wind::BREEZE.to_uniform(1.3);
        let phase = plant_phase(2.0, -1.0);
        assert_eq!(wind_sway(0.0, phase, &uniform), 0.0);
        assert_eq!(wind_sway(-0.5, phase, &uniform), 0.0);
        let mid = wind_sway(0.5, phase, &uniform).abs();
        let tip = wind_sway(1.0, phase, &uniform).abs();
        assert!(tip > mid, "tip {tip} not past mid {mid}");
    }

    #[test]
    fn sway_is_bounded_by_amplitude_times_height() {
        let uniform = Wind::BREEZE.to_uniform(9.9);
        let phase = plant_phase(-3.0, 4.0);
        let bound = (1.0 + uniform.turbulence) * uniform.strength * 1.0;
        assert!(wind_sway(1.0, phase, &uniform).abs() <= bound + 1e-6);
    }

    #[test]
    fn different_plants_sway_differently_at_the_same_time() {
        let uniform = Wind::BREEZE.to_uniform(2.0);
        let a = wind_sway(1.0, plant_phase(0.0, 0.0), &uniform);
        let b = wind_sway(1.0, plant_phase(5.0, 5.0), &uniform);
        assert_ne!(a, b);
    }

    #[test]
    fn zero_turbulence_at_time_zero_is_pure_primary_wave() {
        let uniform = Wind {
            direction: Vec2::X,
            strength: 0.2,
            frequency: 3.0,
            turbulence: 0.0,
        }
        .to_uniform(0.0);
        let phase = 0.9_f32;
        let expected = phase.sin() * 0.2 * 1.0;
        assert!((wind_sway(1.0, phase, &uniform) - expected).abs() < 1e-6);
    }
}
