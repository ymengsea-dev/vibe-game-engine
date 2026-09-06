//! Billboard particle rendering: GPU-instanced camera-facing quads, drawn
//! inside the scene pass into the HDR target so the post-processing bloom
//! and tonemap treat them like any other radiance.
//!
//! This crate owns only the GPU side — the instance format
//! ([`ParticleInstance`]), the camera billboard-basis uniform
//! ([`ParticleCameraUniform`]), the two pipelines ([`ParticlePipelines`]:
//! one alpha-blended, one additive), and the per-frame draw bundle
//! ([`ParticleFrame`]). The simulation (emitters, spawning, integration,
//! ageing) lives in `engine_ecs::particles`, which produces the
//! `ParticleInstance` slices this module draws.
//!
//! Both pipelines run `particle.wgsl` unchanged — it outputs premultiplied
//! colour, so the alpha pipeline blends `(One, OneMinusSrcAlpha)` and the
//! additive one `(One, One)` and both composite correctly. Particles use
//! no depth buffer (the scene pass has none) and no texture (the fragment
//! shader shapes each quad into a soft round blob procedurally); textured
//! and animated particles are future work.

use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::gpu::GpuContext;

const PARTICLE_SHADER_SOURCE: &str = include_str!("shaders/particle.wgsl");

/// One particle's per-instance GPU record: where it is, how big, its
/// linear-RGBA colour (alpha included), and its in-plane roll in radians.
///
/// `#[repr(C)]` + [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make it safely
/// castable to bytes for GPU upload, the same pattern as
/// [`crate::SpriteInstance`]. 48 bytes; the trailing padding keeps the
/// stride a multiple of 16.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleInstance {
    /// World-space centre of the billboard.
    pub position: [f32; 3],
    /// World-space edge length of the (square) billboard.
    pub size: f32,
    /// Linear RGBA. `rgb` is the emitted colour; `a` scales the soft-blob
    /// mask, so `0.0` is invisible and `1.0` is the shader's full opacity.
    pub color: [f32; 4],
    /// In-plane roll, radians, counter-clockwise.
    pub rotation: f32,
    /// Padding to a 16-byte stride. Not a vertex attribute.
    pub _padding: [f32; 3],
}

impl ParticleInstance {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32,
        2 => Float32x4,
        3 => Float32,
    ];

    /// This record's wgpu buffer layout — `step_mode: Instance`, attribute
    /// locations `0..=3` (the quad itself needs no vertex buffer, so there
    /// is no `Vertex` layout sharing the slot the way sprites do).
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<ParticleInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// GPU-layout uniform for the particle pipelines' `@group(0)`: the
/// view-projection matrix plus the camera's world-space right/up axes, so
/// the vertex stage can expand each instance point into a quad that always
/// faces the camera.
///
/// `camera_right`/`camera_up` are `vec4` (not `vec3`) purely for WGSL
/// uniform alignment; `w` is unused. 96 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleCameraUniform {
    /// Column-major view-projection matrix, matching WGSL `mat4x4<f32>`.
    pub view_projection: [[f32; 4]; 4],
    /// World-space camera right axis (`xyz`; `w` unused).
    pub camera_right: [f32; 4],
    /// World-space camera up axis (`xyz`; `w` unused).
    pub camera_up: [f32; 4],
}

impl ParticleCameraUniform {
    /// Builds the uniform from `camera`: its view-projection matrix, and
    /// the camera-space right/up axes read back out of the view matrix
    /// (its upper-left 3x3 is orthonormal, so row 0 is world-space right
    /// and row 1 is world-space up).
    pub fn from_camera(camera: &Camera) -> Self {
        let view = camera.view_matrix();
        let right = view.row(0).truncate().normalize_or_zero();
        let up = view.row(1).truncate().normalize_or_zero();
        Self {
            view_projection: camera.view_projection_matrix().to_cols_array_2d(),
            camera_right: [right.x, right.y, right.z, 0.0],
            camera_up: [up.x, up.y, up.z, 0.0],
        }
    }
}

impl From<&Camera> for ParticleCameraUniform {
    fn from(camera: &Camera) -> Self {
        Self::from_camera(camera)
    }
}

/// One compiled billboard-particle pipeline. Two of these live in
/// [`ParticlePipelines`], identical but for their blend state.
pub struct ParticlePipeline {
    pub(crate) render_pipeline: wgpu::RenderPipeline,
}

/// The alpha-blended and additive particle pipelines, plus the shared
/// `@group(0)` layout their [`ParticleCameraBinding`] is built against.
///
/// Built once ([`GpuContext::create_particle_pipelines`]); pick per emitter
/// via `engine_ecs::BlendMode`.
pub struct ParticlePipelines {
    /// Standard transparency — premultiplied `(One, OneMinusSrcAlpha)`.
    /// Order-dependent; draw back-to-front for exact results (the MVP
    /// draws in emitter/pool order).
    pub alpha: ParticlePipeline,
    /// Additive — `(One, One)`. Order-independent, pairs well with bloom
    /// for embers/sparks/magic.
    pub additive: ParticlePipeline,
    camera_bind_group_layout: wgpu::BindGroupLayout,
}

/// A [`ParticleCameraUniform`] buffer plus the bind group exposing it to
/// both particle pipelines at `@group(0)`. Rewrite the buffer each frame
/// the camera moves via [`GpuContext::write_particle_camera_binding`].
pub struct ParticleCameraBinding {
    /// The GPU buffer backing the uniform. Kept for per-frame updates.
    pub buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
}

/// A `VERTEX`-usage buffer of [`ParticleInstance`]s for one frame's draw.
/// Rebuilt every frame (the particle set changes constantly); a persistent
/// growable buffer is future work, same as the other instanced paths.
pub struct ParticleInstanceBuffer {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) count: u32,
}

/// Everything [`GpuContext::render_scene`] needs to draw one frame of
/// particles: the pipelines, the camera binding, and the two instance
/// slices (already split by blend mode by `engine_ecs::extract_particles`).
/// `render_scene` uploads each non-empty slice to a transient buffer and
/// draws it with the matching pipeline, last in the scene pass.
#[derive(Clone, Copy)]
pub struct ParticleFrame<'a> {
    /// The alpha + additive pipelines.
    pub pipelines: &'a ParticlePipelines,
    /// The billboard-basis camera binding (keep it updated per frame).
    pub camera: &'a ParticleCameraBinding,
    /// Alpha-blended particles, drawn first.
    pub alpha: &'a [ParticleInstance],
    /// Additive particles, drawn after the alpha ones.
    pub additive: &'a [ParticleInstance],
}

impl<'a> ParticleFrame<'a> {
    /// Whether there is nothing to draw this frame (both slices empty).
    pub fn is_empty(&self) -> bool {
        self.alpha.is_empty() && self.additive.is_empty()
    }
}

impl GpuContext {
    /// Compiles `particle.wgsl` into a [`ParticlePipelines`] pair (alpha +
    /// additive) targeting the HDR scene format at the scene pass's sample
    /// count.
    pub fn create_particle_pipelines(&self, label: &str) -> ParticlePipelines {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(PARTICLE_SHADER_SOURCE.into()),
        });

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("particle camera bind group layout"),
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
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });

        // Both pipelines are identical apart from the colour-target blend;
        // the shader outputs premultiplied colour so both are correct.
        let premultiplied_alpha = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let make = |blend: wgpu::BlendState, variant: &str| {
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("{label} ({variant})")),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(ParticleInstance::layout())],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: crate::pipeline::HDR_TEXTURE_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    // Billboards always face the camera; winding is
                    // irrelevant, so don't cull.
                    cull_mode: None,
                    ..Default::default()
                },
                // Transparent billboards: occluded by geometry, but they
                // must not write depth or they would hide each other.
                depth_stencil: Some(crate::pipeline::transparent_depth_state()),
                multisample: self.scene_multisample_state(),
                multiview_mask: None,
                cache: None,
            });
            ParticlePipeline {
                render_pipeline: pipeline,
            }
        };

        ParticlePipelines {
            alpha: make(premultiplied_alpha, "alpha"),
            additive: make(additive, "additive"),
            camera_bind_group_layout,
        }
    }

    /// Creates a [`ParticleCameraBinding`]: a [`ParticleCameraUniform`]
    /// buffer seeded from `camera`, plus the bind group exposing it to both
    /// particle pipelines.
    pub fn create_particle_camera_binding(
        &self,
        pipelines: &ParticlePipelines,
        camera: &Camera,
    ) -> ParticleCameraBinding {
        let buffer = self.create_uniform_buffer(
            "particle camera uniform",
            &ParticleCameraUniform::from(camera),
        );
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle camera bind group"),
            layout: &pipelines.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        ParticleCameraBinding { buffer, bind_group }
    }

    /// Rewrites `binding`'s uniform buffer from `camera`'s current
    /// view-projection and billboard axes. Call once per frame the camera
    /// moves, before [`GpuContext::render_scene`].
    pub fn write_particle_camera_binding(&self, binding: &ParticleCameraBinding, camera: &Camera) {
        self.write_uniform_buffer(&binding.buffer, &ParticleCameraUniform::from(camera));
    }

    /// Uploads `instances` as a `VERTEX`-usage [`ParticleInstanceBuffer`],
    /// or `None` if `instances` is empty (a zero-length buffer is invalid).
    pub fn create_particle_instance_buffer(
        &self,
        label: &str,
        instances: &[ParticleInstance],
    ) -> Option<ParticleInstanceBuffer> {
        if instances.is_empty() {
            return None;
        }
        let buffer = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(instances),
                usage: wgpu::BufferUsages::VERTEX,
            });
        Some(ParticleInstanceBuffer {
            buffer,
            count: instances.len() as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn instance_layout_matches_struct_and_is_instance_stepped() {
        let layout = ParticleInstance::layout();
        assert_eq!(
            layout.array_stride,
            size_of::<ParticleInstance>() as wgpu::BufferAddress
        );
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 4);
    }

    #[test]
    fn instance_and_uniform_sizes_are_16_byte_multiples() {
        assert_eq!(size_of::<ParticleInstance>() % 16, 0);
        assert_eq!(size_of::<ParticleCameraUniform>() % 16, 0);
    }

    #[test]
    fn uniform_is_96_bytes() {
        assert_eq!(size_of::<ParticleCameraUniform>(), 96);
    }

    #[test]
    fn instance_round_trips_through_bytes() {
        let instance = ParticleInstance {
            position: [1.0, 2.0, 3.0],
            size: 0.5,
            color: [0.1, 0.2, 0.3, 0.4],
            rotation: 1.25,
            _padding: [0.0; 3],
        };
        let bytes = bytemuck::bytes_of(&instance);
        let back: ParticleInstance = *bytemuck::from_bytes(bytes);
        assert_eq!(instance, back);
    }

    #[test]
    fn camera_uniform_basis_is_orthonormal_and_faces_the_camera() {
        let camera = Camera::new(Vec3::new(3.0, 2.0, 5.0), Vec3::ZERO, 16.0 / 9.0);
        let uniform = ParticleCameraUniform::from(&camera);
        let right = Vec3::from_slice(&uniform.camera_right[..3]);
        let up = Vec3::from_slice(&uniform.camera_up[..3]);

        assert!((right.length() - 1.0).abs() < 1e-5);
        assert!((up.length() - 1.0).abs() < 1e-5);
        assert!(right.dot(up).abs() < 1e-5);

        // Both axes lie in the plane perpendicular to the view direction.
        let view_dir = (camera.target - camera.eye).normalize();
        assert!(right.dot(view_dir).abs() < 1e-5);
        assert!(up.dot(view_dir).abs() < 1e-5);
    }

    #[test]
    fn camera_uniform_carries_the_view_projection() {
        let camera = Camera::new(Vec3::new(1.0, 1.0, 4.0), Vec3::ZERO, 1.0);
        assert_eq!(
            ParticleCameraUniform::from(&camera).view_projection,
            camera.view_projection_matrix().to_cols_array_2d()
        );
    }
}
