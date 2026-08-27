//! First WGSL render pipeline: camera-transformed, PBR-material-textured
//! geometry.

use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::camera::CameraUniform;
use crate::gpu::GpuContext;
use crate::hdr::TonemapUniform;
use crate::light::LightsUniform;
use crate::material::MaterialUniform;
use crate::mesh::{Mesh, Vertex};
use crate::model::ModelUniform;
use crate::shadow::{SHADOW_MAP_SIZE, ShadowUniform};
use crate::skybox::SkyboxUniform;
use crate::texture::Texture;

const SHADER_SOURCE: &str = include_str!("shaders/pbr.wgsl");
const SHADOW_SHADER_SOURCE: &str = include_str!("shaders/shadow.wgsl");
const SKYBOX_SHADER_SOURCE: &str = include_str!("shaders/skybox.wgsl");
const TONEMAP_SHADER_SOURCE: &str = include_str!("shaders/tonemap.wgsl");
const DEBUG_LINE_SHADER_SOURCE: &str = include_str!("shaders/debug_line.wgsl");
const SHADOW_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The main color pass's off-screen render target format — 16-bit float
/// per channel, enough headroom for lighting math above `1.0` without the
/// `float32-filterable` device feature `Rgba32Float` sampling would need.
const HDR_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// A compiled render pipeline plus the bind group layouts its shader
/// expects the camera uniform (`@group(0)`), material (`@group(1)`:
/// factors uniform, base color texture, sampler), per-object model matrix
/// (`@group(2)`), and scene lights (`@group(3)`) in.
///
/// Only one pipeline exists right now. Multiple
/// pipelines/materials-per-mesh-primitive are future work once there's a
/// reason for more than one shader.
pub struct Pipeline {
    render_pipeline: wgpu::RenderPipeline,
    camera_bind_group_layout: wgpu::BindGroupLayout,
    material_bind_group_layout: wgpu::BindGroupLayout,
    model_bind_group_layout: wgpu::BindGroupLayout,
    lights_bind_group_layout: wgpu::BindGroupLayout,
}

/// A camera's uniform buffer plus the bind group that exposes it to a
/// [`Pipeline`]'s shader at `@group(0)`.
pub struct CameraBinding {
    /// The GPU buffer backing this binding. Kept around so callers can
    /// update it per frame via [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    /// `pub(crate)` rather than private: [`crate::sprite`] builds and
    /// reads this type too (against its own, differently-shaped
    /// [`crate::SpritePipeline`] camera bind group layout), so the same
    /// "camera uniform + bind group" wrapper serves both pipelines
    /// instead of a duplicate type.
    pub(crate) bind_group: wgpu::BindGroup,
}

/// A material's factors uniform buffer, base color texture, and the bind
/// group exposing both to a [`Pipeline`]'s shader at `@group(1)`.
pub struct MaterialBinding {
    /// The GPU buffer backing the factors uniform. Kept around so callers
    /// can update it (e.g. after editing a material) via
    /// [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// An object's model-matrix uniform buffer plus the bind group that
/// exposes it to a [`Pipeline`]'s shader at `@group(2)`.
///
/// One buffer per object rather than a shared, dynamic-offset buffer —
/// simple and correct for the handful of objects this engine can draw
/// today; revisit if per-object bind group count becomes a real cost.
pub struct ModelBinding {
    /// The GPU buffer backing this binding. Kept around so callers can
    /// update it (e.g. once per frame, from an entity's current
    /// `Transform`) via [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The scene's lights uniform buffer, its shadow map's texture/sampler/
/// light-space matrix, and the bind group exposing all of it to a
/// [`Pipeline`]'s shader at `@group(3)`. One per frame (shared across all
/// [`Drawable`]s), like [`CameraBinding`].
pub struct LightsBinding {
    /// The GPU buffer backing the lights uniform. Kept around so callers
    /// can update it (e.g. once per frame, if lights move) via
    /// [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// A depth-only pipeline that renders a scene's depth from a directional
/// light's point of view, into a [`ShadowMap`].
///
/// Reuses [`Pipeline`]'s model bind group layout (`@group(1)` here matches
/// `@group(2)` there) so a [`Drawable`]'s existing [`ModelBinding`] works
/// unmodified in both passes.
pub struct ShadowPipeline {
    render_pipeline: wgpu::RenderPipeline,
    light_bind_group_layout: wgpu::BindGroupLayout,
}

/// A directional light's shadow depth texture plus the resources needed
/// to both render it (with a [`ShadowPipeline`]) and sample it back in
/// [`Pipeline`]'s main color pass.
pub struct ShadowMap {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// The GPU buffer backing the light-space matrix uniform. Kept around
    /// so callers can update it (e.g. if the shadow-casting light moves)
    /// via [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    light_bind_group: wgpu::BindGroup,
}

/// A pipeline that draws a procedural sky as a full-screen triangle, no
/// vertex/index buffer needed. Its shader expects a single uniform
/// ([`SkyboxUniform`]) at `@group(0)`.
pub struct SkyboxPipeline {
    render_pipeline: wgpu::RenderPipeline,
    uniform_bind_group_layout: wgpu::BindGroupLayout,
}

/// A [`SkyboxUniform`] buffer plus the bind group exposing it to a
/// [`SkyboxPipeline`]'s shader at `@group(0)`.
pub struct SkyboxBinding {
    /// The GPU buffer backing this binding. Kept around so callers can
    /// update it (e.g. after a resize, since the inverse view-projection
    /// depends on aspect ratio) via [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The main color pass's off-screen HDR render target: a floating-point
/// texture the scene renders into instead of the LDR swapchain directly,
/// so radiance above `1.0` survives to the tonemap pass instead of
/// clipping on write.
///
/// Sized to match the window — [`GpuContext::create_hdr_target`] must be
/// called again (and its [`TonemapBinding`] rebuilt, since a bind group
/// can't be repointed at a new texture view in place) whenever the window
/// resizes.
pub struct HdrTarget {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// A pipeline that reads a [`HdrTarget`] and tonemaps it into the
/// swapchain's displayable range as a full-screen triangle, no
/// vertex/index buffer needed (same technique as [`SkyboxPipeline`]).
pub struct TonemapPipeline {
    render_pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

/// A [`HdrTarget`] (texture view + sampler) and [`TonemapUniform`] buffer,
/// bundled into the bind group [`TonemapPipeline`]'s shader expects at
/// `@group(0)`.
pub struct TonemapBinding {
    /// The GPU buffer backing the exposure uniform. Kept around so
    /// callers can update it (e.g. to change exposure) via
    /// [`GpuContext::write_uniform_buffer`].
    pub buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// One point of a physics debug-visualization line segment: world-space
/// position plus a pre-computed linear RGBA color. Two of these (drawn
/// with [`wgpu::PrimitiveTopology::LineList`]) make one line.
///
/// No lighting, no material — debug lines are drawn exactly the color
/// they're given (e.g. from `engine_physics::DebugLine`, whose docs cover
/// where the color comes from).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DebugLineVertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Linear RGBA, `[0, 1]` per channel.
    pub color: [f32; 4],
}

impl DebugLineVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<DebugLineVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// A pipeline that draws pre-colored line segments
/// ([`DebugLineVertex`] pairs, [`wgpu::PrimitiveTopology::LineList`]) —
/// physics collider/joint wireframes and the like. Reuses [`Pipeline`]'s
/// camera bind group layout (`@group(0)` here matches `@group(0)` there)
/// so an existing [`CameraBinding`] works unmodified — see
/// `debug_line.wgsl`'s top comment for why its declared uniform struct
/// can be smaller than [`CameraUniform`] and still bind correctly.
pub struct DebugLinePipeline {
    render_pipeline: wgpu::RenderPipeline,
}

/// Everything needed to draw one object in [`GpuContext::render_scene`]:
/// its model transform, its material, and its geometry.
#[derive(Clone, Copy)]
pub struct Drawable<'a> {
    /// This object's model matrix binding.
    pub model: &'a ModelBinding,
    /// This object's material binding.
    pub material: &'a MaterialBinding,
    /// This object's geometry.
    pub mesh: &'a Mesh,
}

impl GpuContext {
    /// Compiles `pbr.wgsl` into a [`Pipeline`] targeting this context's
    /// surface format.
    pub fn create_pbr_pipeline(&self, label: &str) -> Pipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("camera bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Vertex stage: transforms positions by view_proj.
                    // Fragment stage: reads view_position for the BRDF's
                    // view vector.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let material_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("material bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let model_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("model bind group layout"),
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

        let lights_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("lights bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    // Shadow map for the first directional light — see
                    // `pbr.wgsl`'s `shadow_factor`.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&material_bind_group_layout),
                Some(&model_bind_group_layout),
                Some(&lights_bind_group_layout),
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
                buffers: &[Some(Vertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // Renders into the HDR offscreen target ([`HdrTarget`]),
                // not the swapchain directly — the tonemap pass converts
                // that down to the swapchain's format afterward.
                targets: &[Some(HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                // Our built-in geometry (see `cube()`) winds each face
                // counter-clockwise as seen from outside, matching the
                // default front face; culling back faces gives a
                // correct-looking convex mesh without needing a depth
                // buffer. Revisit once concave/non-convex meshes exist.
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Pipeline {
            render_pipeline,
            camera_bind_group_layout,
            material_bind_group_layout,
            model_bind_group_layout,
            lights_bind_group_layout,
        }
    }

    /// Creates a uniform buffer + bind group exposing `uniform` to
    /// `pipeline`'s shader.
    pub fn create_camera_binding(
        &self,
        pipeline: &Pipeline,
        uniform: &CameraUniform,
    ) -> CameraBinding {
        let buffer = self.create_uniform_buffer("camera uniform", uniform);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera bind group"),
            layout: &pipeline.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        CameraBinding { buffer, bind_group }
    }

    /// Creates a uniform buffer + bind group exposing `texture` (base
    /// color) and `material` (factors) to `pipeline`'s shader.
    pub fn create_material_binding(
        &self,
        pipeline: &Pipeline,
        texture: &Texture,
        material: &MaterialUniform,
    ) -> MaterialBinding {
        let buffer = self.create_uniform_buffer("material uniform", material);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material bind group"),
            layout: &pipeline.material_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        MaterialBinding { buffer, bind_group }
    }

    /// Creates a uniform buffer + bind group exposing `uniform` (a model
    /// matrix) to `pipeline`'s shader.
    pub fn create_model_binding(
        &self,
        pipeline: &Pipeline,
        uniform: &ModelUniform,
    ) -> ModelBinding {
        let buffer = self.create_uniform_buffer("model uniform", uniform);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("model bind group"),
            layout: &pipeline.model_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        ModelBinding { buffer, bind_group }
    }

    /// Creates a uniform buffer + bind group exposing `uniform` (the
    /// scene's lights) and `shadow_map` (the first directional light's
    /// shadow map) to `pipeline`'s shader.
    pub fn create_lights_binding(
        &self,
        pipeline: &Pipeline,
        uniform: &LightsUniform,
        shadow_map: &ShadowMap,
    ) -> LightsBinding {
        let buffer = self.create_uniform_buffer("lights uniform", uniform);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lights bind group"),
            layout: &pipeline.lights_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&shadow_map.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&shadow_map.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: shadow_map.buffer.as_entire_binding(),
                },
            ],
        });
        LightsBinding { buffer, bind_group }
    }

    /// Compiles `shadow.wgsl` into a [`ShadowPipeline`], reusing
    /// `pipeline`'s model bind group layout so [`ModelBinding`]s work
    /// unmodified in both the shadow and main color passes.
    pub fn create_shadow_pipeline(&self, pipeline: &Pipeline, label: &str) -> ShadowPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(SHADOW_SHADER_SOURCE.into()),
        });

        let light_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("shadow light bind group layout"),
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
                Some(&light_bind_group_layout),
                Some(&pipeline.model_bind_group_layout),
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
                buffers: &[Some(Vertex::layout())],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState {
                // Cull front faces (not back, unlike the main pipeline):
                // writing the *back*-face depth of each occluder reduces
                // shadow acne on lit front faces without needing an
                // aggressive depth bias. Standard peter-panning trade-off.
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        ShadowPipeline {
            render_pipeline,
            light_bind_group_layout,
        }
    }

    /// Creates a [`ShadowMap`]: a [`crate::shadow::SHADOW_MAP_SIZE`]-square
    /// depth texture plus the light-space matrix uniform buffer and bind
    /// group `shadow_pipeline` needs to render into it.
    pub fn create_shadow_map(
        &self,
        shadow_pipeline: &ShadowPipeline,
        light_space_matrix: Mat4,
    ) -> ShadowMap {
        let device = self.device();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d {
                width: SHADOW_MAP_SIZE,
                height: SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow map sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });

        let buffer = self.create_uniform_buffer(
            "shadow light-space uniform",
            &ShadowUniform::from(light_space_matrix),
        );
        let light_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shadow light bind group"),
            layout: &shadow_pipeline.light_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        ShadowMap {
            view,
            sampler,
            buffer,
            light_bind_group,
        }
    }

    /// Compiles `skybox.wgsl` into a [`SkyboxPipeline`] targeting this
    /// context's surface format.
    pub fn create_skybox_pipeline(&self, label: &str) -> SkyboxPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(SKYBOX_SHADER_SOURCE.into()),
        });

        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("skybox bind group layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
            bind_group_layouts: &[Some(&uniform_bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // No vertex buffer: `vs_main` generates the full-screen
                // triangle's positions from `@builtin(vertex_index)`.
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // Drawn in the same render pass as `Pipeline`'s geometry,
                // into the HDR offscreen target — see its `targets` for
                // why.
                targets: &[Some(HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                // The full-screen triangle's winding doesn't matter (it
                // always faces the camera by construction) — don't cull
                // either way.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        SkyboxPipeline {
            render_pipeline,
            uniform_bind_group_layout,
        }
    }

    /// Creates a uniform buffer + bind group exposing `uniform` to
    /// `pipeline`'s shader.
    pub fn create_skybox_binding(
        &self,
        pipeline: &SkyboxPipeline,
        uniform: &SkyboxUniform,
    ) -> SkyboxBinding {
        let buffer = self.create_uniform_buffer("skybox uniform", uniform);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("skybox bind group"),
            layout: &pipeline.uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        SkyboxBinding { buffer, bind_group }
    }

    /// Creates a [`HdrTarget`] sized `width` by `height` (the window's
    /// current size) — a floating-point offscreen texture the main color
    /// pass renders into.
    pub fn create_hdr_target(&self, width: u32, height: u32) -> HdrTarget {
        let device = self.device();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hdr target sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        HdrTarget { view, sampler }
    }

    /// Compiles `tonemap.wgsl` into a [`TonemapPipeline`] targeting this
    /// context's surface format.
    pub fn create_tonemap_pipeline(&self, label: &str) -> TonemapPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(TONEMAP_SHADER_SOURCE.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tonemap bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(self.config().format.into())],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        TonemapPipeline {
            render_pipeline,
            bind_group_layout,
        }
    }

    /// Creates a [`TonemapBinding`]: an exposure uniform buffer plus the
    /// bind group exposing it and `hdr_target`'s view/sampler to
    /// `pipeline`'s shader.
    ///
    /// Must be recreated (along with `hdr_target` itself) whenever the
    /// window resizes — a bind group can't be repointed at a new texture
    /// view in place.
    pub fn create_tonemap_binding(
        &self,
        pipeline: &TonemapPipeline,
        hdr_target: &HdrTarget,
        exposure: f32,
    ) -> TonemapBinding {
        let buffer = self.create_uniform_buffer("tonemap uniform", &TonemapUniform::from(exposure));
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap bind group"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&hdr_target.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&hdr_target.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        TonemapBinding { buffer, bind_group }
    }

    /// Compiles `debug_line.wgsl` into a [`DebugLinePipeline`], reusing
    /// `pipeline`'s camera bind group layout so an existing
    /// [`CameraBinding`] works unmodified.
    pub fn create_debug_line_pipeline(
        &self,
        pipeline: &Pipeline,
        label: &str,
    ) -> DebugLinePipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(DEBUG_LINE_SHADER_SOURCE.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[Some(&pipeline.camera_bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(DebugLineVertex::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        DebugLinePipeline { render_pipeline }
    }

    /// Renders one frame through a [`crate::RenderGraph`] of three
    /// declared passes — `"shadow"` (writes `"shadow_map"`), `"scene"`
    /// (reads `"shadow_map"`, writes `"hdr_target"`), `"tonemap"` (reads
    /// `"hdr_target"`, writes `"swapchain"`) — resolved to that exact
    /// shadow-then-scene-then-tonemap order (each pass depends on the
    /// previous one's output), the same sequence this function hardcoded
    /// before the render graph existed. Behavior is unchanged; only how
    /// the sequence is expressed is: future passes (e.g. Stage 5's
    /// post-processing) slot in by declaring what they read/write, not by
    /// another rewrite of this function.
    ///
    /// The `"shadow"` pass (`shadow_pipeline`/`shadow_map`, from the
    /// shadow-casting light's point of view) runs first. The `"scene"`
    /// pass draws the procedural sky (`skybox_pipeline`/`skybox`) across
    /// the whole viewport first, then every [`Drawable`] in `drawables`
    /// on top of it with `pipeline`, `camera`, and `lights` (which
    /// samples `shadow_map` back in the fragment shader; see
    /// `pbr.wgsl`'s `shadow_factor`), optionally followed by
    /// `debug_lines` (`Some((pipeline, vertices))` — e.g. from
    /// `engine_physics::debug_render_lines`, converted to
    /// [`DebugLineVertex`] pairs; `None` or an empty slice draws nothing
    /// extra) drawn on top of everything else — into `hdr_target` rather
    /// than the swapchain directly. The `"tonemap"` pass
    /// (`tonemap_pipeline`/`tonemap_binding`) compresses `hdr_target`
    /// into the swapchain's displayable range. All three share one
    /// command buffer and submit together.
    ///
    /// An empty `drawables` still renders every pass and presents — this
    /// is the "nothing to draw yet" case, not an error.
    ///
    /// Skip/error handling for surface texture acquisition matches
    /// [`GpuContext::render_clear`] — see its docs for the full list of
    /// transient conditions treated as "skip this frame".
    #[allow(
        clippy::too_many_arguments,
        reason = "one binding pair per render pass input; grouping them loses the parallel with the *Pipeline/*Binding types they name"
    )]
    pub fn render_scene(
        &self,
        pipeline: &Pipeline,
        shadow_pipeline: &ShadowPipeline,
        shadow_map: &ShadowMap,
        skybox_pipeline: &SkyboxPipeline,
        skybox: &SkyboxBinding,
        hdr_target: &HdrTarget,
        tonemap_pipeline: &TonemapPipeline,
        tonemap_binding: &TonemapBinding,
        camera: &CameraBinding,
        lights: &LightsBinding,
        drawables: &[Drawable<'_>],
        debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
    ) -> Result<(), crate::error::RendererError> {
        let Some(surface_texture) = self.acquire_frame() else {
            return Ok(());
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VGE scene frame encoder"),
            });

        // Built once here (rather than inside the "scene" pass below) so
        // it outlives the render pass that borrows it — a `wgpu::Buffer`
        // created inside the pass's closure wouldn't live long enough.
        // `None` for an empty slice too: an empty vertex buffer is legal
        // but pointless to allocate.
        let debug_vertex_buffer =
            debug_lines
                .filter(|(_, lines)| !lines.is_empty())
                .map(|(_, lines)| {
                    self.device()
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("debug line vertices"),
                            contents: bytemuck::cast_slice(lines),
                            usage: wgpu::BufferUsages::VERTEX,
                        })
                });

        let mut graph = crate::RenderGraph::new();

        graph.add_pass("shadow", &[], &["shadow_map"], |encoder| {
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow_map.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            shadow_pass.set_pipeline(&shadow_pipeline.render_pipeline);
            shadow_pass.set_bind_group(0, &shadow_map.light_bind_group, &[]);
            for drawable in drawables {
                shadow_pass.set_bind_group(1, &drawable.model.bind_group, &[]);
                shadow_pass.set_vertex_buffer(0, drawable.mesh.vertex_buffer.slice(..));
                shadow_pass.set_index_buffer(
                    drawable.mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                shadow_pass.draw_indexed(0..drawable.mesh.index_count, 0, 0..1);
            }
        });

        graph.add_pass("scene", &["shadow_map"], &["hdr_target"], |encoder| {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    // Into the HDR offscreen target, not the swapchain —
                    // the "tonemap" pass writes the swapchain.
                    view: &hdr_target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Sky first: a full-screen triangle painting every pixel, so
            // opaque geometry drawn after it simply overwrites the sky
            // where it's in front (no depth buffer needed for this —
            // draw order alone is correct here, same as the rest of this
            // pass's opaque geometry; see `Pipeline`'s docs).
            render_pass.set_pipeline(&skybox_pipeline.render_pipeline);
            render_pass.set_bind_group(0, &skybox.bind_group, &[]);
            render_pass.draw(0..3, 0..1);

            render_pass.set_pipeline(&pipeline.render_pipeline);
            render_pass.set_bind_group(0, &camera.bind_group, &[]);
            render_pass.set_bind_group(3, &lights.bind_group, &[]);

            for drawable in drawables {
                render_pass.set_bind_group(1, &drawable.material.bind_group, &[]);
                render_pass.set_bind_group(2, &drawable.model.bind_group, &[]);
                render_pass.set_vertex_buffer(0, drawable.mesh.vertex_buffer.slice(..));
                render_pass.set_index_buffer(
                    drawable.mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                render_pass.draw_indexed(0..drawable.mesh.index_count, 0, 0..1);
            }

            // Debug lines last: always drawn on top of everything else in
            // the pass (there's no depth buffer to test against — see
            // `Pipeline`'s docs — so draw order alone decides visibility,
            // which is exactly what a debug overlay wants).
            if let Some((debug_pipeline, lines)) = debug_lines
                && let Some(buffer) = &debug_vertex_buffer
            {
                render_pass.set_pipeline(&debug_pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..lines.len() as u32, 0..1);
            }
        });

        graph.add_pass("tonemap", &["hdr_target"], &["swapchain"], |encoder| {
            let mut tonemap_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE tonemap pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Fully overwritten by the full-screen triangle
                        // below; `Clear` is just a defined starting state.
                        load: wgpu::LoadOp::Clear(self.clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            tonemap_pass.set_pipeline(&tonemap_pipeline.render_pipeline);
            tonemap_pass.set_bind_group(0, &tonemap_binding.bind_group, &[]);
            tonemap_pass.draw(0..3, 0..1);
        });

        graph.execute(&mut encoder)?;

        self.queue().submit(std::iter::once(encoder.finish()));
        self.present(surface_texture);
        Ok(())
    }
}
