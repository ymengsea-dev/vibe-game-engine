//! First WGSL render pipeline: camera-transformed, PBR-material-textured
//! geometry.

use glam::Mat4;
use wgpu::util::DeviceExt;

use crate::camera::CameraUniform;
use crate::gpu::GpuContext;
use crate::light::LightsUniform;
use crate::material::MaterialUniform;
use crate::mesh::{Mesh, Vertex};
use crate::model::ModelUniform;
use crate::shadow::{SHADOW_MAP_SIZE, ShadowUniform};
use crate::skybox::SkyboxUniform;
use crate::texture::Texture;

/// Shared PBR bindings, lighting, and fragment stage. One vertex-stage
/// file ([`PBR_VS_SOURCE`] here, or the skinned one in [`crate::skinning`])
/// is concatenated onto it at pipeline creation — WGSL has no `#include`,
/// and duplicating ~200 lines of BRDF per variant is worse.
pub(crate) const PBR_COMMON_SOURCE: &str = include_str!("shaders/pbr_common.wgsl");
const PBR_VS_SOURCE: &str = include_str!("shaders/pbr_vs.wgsl");
const SHADOW_SHADER_SOURCE: &str = include_str!("shaders/shadow.wgsl");
const SKYBOX_SHADER_SOURCE: &str = include_str!("shaders/skybox.wgsl");
const DEBUG_LINE_SHADER_SOURCE: &str = include_str!("shaders/debug_line.wgsl");
const SHADOW_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Depth format for the main scene pass.
///
/// Separate constant from the shadow map's own depth format despite
/// matching it
/// today: the shadow map's is driven by what a comparison sampler
/// accepts, this one by precision needs, and the two are free to
/// diverge.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Depth state for opaque geometry: test against what's already there,
/// and write, so nearer fragments win regardless of draw order.
pub(crate) fn opaque_depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth state for transparent geometry: occluded by opaque surfaces in
/// front of it, but never occluding anything itself (writing depth from a
/// see-through fragment would hide whatever is behind it).
pub(crate) fn transparent_depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth state for passes that must ignore depth entirely — the skybox
/// (drawn first, must lose to all geometry) and editor overlays like the
/// gizmo (drawn last, must stay grabbable through geometry).
pub(crate) fn overlay_depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}
/// The main color pass's off-screen render target format — 16-bit float
/// per channel, enough headroom for lighting math above `1.0` without the
/// `float32-filterable` device feature `Rgba32Float` sampling would need.
pub(crate) const HDR_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

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
    /// The same pipeline with alpha blending on and depth writes off,
    /// used for the sorted transparent pass. Built alongside the opaque
    /// one so both always agree on layouts and shader.
    transparent_pipeline: wgpu::RenderPipeline,
    camera_bind_group_layout: wgpu::BindGroupLayout,
    material_bind_group_layout: wgpu::BindGroupLayout,
    model_bind_group_layout: wgpu::BindGroupLayout,
    lights_bind_group_layout: wgpu::BindGroupLayout,
    /// 1x1 stand-ins for the optional material maps, created once here
    /// rather than per material — a scene with hundreds of props would
    /// otherwise allocate hundreds of identical 1x1 textures.
    neutral: NeutralMaps,
}

/// The optional maps a material can carry beyond its base colour.
///
/// Every field is optional; a `None` samples `Pipeline`'s neutral 1x1
/// instead, which leaves the corresponding term unchanged.
#[derive(Default, Clone, Copy)]
pub struct MaterialMaps<'a> {
    /// Tangent-space normal map.
    pub normal: Option<&'a Texture>,
    /// Emissive colour map, multiplied by `Material::emissive_factor`.
    pub emissive: Option<&'a Texture>,
    /// Ambient occlusion, read from the red channel.
    pub occlusion: Option<&'a Texture>,
}

/// The "this material has no such map" textures: a flat normal, black
/// emissive, and full-white occlusion. Sampling these leaves the
/// corresponding material factor untouched.
struct NeutralMaps {
    normal: Texture,
    emissive: Texture,
    occlusion: Texture,
}

impl Pipeline {
    /// The camera (`@group(0)`) bind group layout — reused unchanged by
    /// [`crate::SkinnedPipeline`].
    pub(crate) fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.camera_bind_group_layout
    }

    /// The material (`@group(1)`) bind group layout — reused unchanged by
    /// [`crate::SkinnedPipeline`], so one [`MaterialBinding`] serves both.
    pub(crate) fn material_layout(&self) -> &wgpu::BindGroupLayout {
        &self.material_bind_group_layout
    }

    /// The lights (`@group(3)`) bind group layout — reused unchanged by
    /// [`crate::SkinnedPipeline`].
    pub(crate) fn lights_layout(&self) -> &wgpu::BindGroupLayout {
        &self.lights_bind_group_layout
    }

    /// The alpha-blended, depth-write-off variant used by the sorted
    /// transparent pass.
    pub(crate) fn transparent(&self) -> &wgpu::RenderPipeline {
        &self.transparent_pipeline
    }
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
    /// Whether this material belongs in the sorted transparent pass.
    ///
    /// Cached CPU-side because the mode itself lives in a GPU buffer the
    /// render path cannot read back, and the split has to happen during
    /// extraction.
    pub transparent: bool,
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
/// so radiance above `1.0` survives to the post-processing stack instead
/// of clipping on write.
///
/// Sized to match the window — [`GpuContext::create_hdr_target`] must be
/// called again (and the [`crate::PostProcessStack`] rebuilt, since a bind
/// group can't be repointed at a new texture view in place) whenever the
/// window resizes.
///
/// When [`GpuContext::msaa_sample_count`] is above `1`, `msaa_view` is a
/// multisampled color texture the scene pass renders into and resolves
/// down into `view` (single-sample, the one the post-processing stack
/// samples). With no MSAA, `msaa_view` is `None` and the scene pass
/// renders straight into `view`.
pub struct HdrTarget {
    view: wgpu::TextureView,
    msaa_view: Option<wgpu::TextureView>,
    depth_view: wgpu::TextureView,
}

impl HdrTarget {
    /// The single-sample, already-resolved color view — what the
    /// post-processing stack samples. (The multisampled `msaa_view`, when
    /// present, is render-only and never sampled directly.)
    pub(crate) fn color_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The scene pass's depth attachment. Created at the same sample
    /// count as whichever color attachment the pass uses (the MSAA view
    /// when present, otherwise the resolve view) — wgpu requires every
    /// attachment in a pass to agree on sample count.
    pub(crate) fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }
}

/// Where a frame composites to: an off-screen texture view.
///
/// A thin borrow wrapper so crates that merely *forward* a render target
/// — `engine_ecs`, which has no `wgpu` dependency of its own — can pass
/// one through without taking on `wgpu` as a public dependency.
///
/// Build one from the view you want written, and make sure its format
/// matches the one the [`crate::PostProcessStack`] was created for (see
/// [`GpuContext::create_post_process_stack_for_format`]).
#[derive(Clone, Copy)]
pub struct RenderTarget<'a> {
    view: &'a wgpu::TextureView,
}

impl<'a> RenderTarget<'a> {
    /// Wraps `view` as a render target.
    pub fn new(view: &'a wgpu::TextureView) -> Self {
        Self { view }
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        self.view
    }
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
    /// The multisample state for pipelines that draw into the main color
    /// pass's [`HdrTarget`] — its `count` is [`GpuContext::msaa_sample_count`].
    /// The shadow (depth-only) and tonemap (into the single-sample
    /// swapchain) pipelines keep the default 1-sample state instead.
    pub(crate) fn scene_multisample_state(&self) -> wgpu::MultisampleState {
        wgpu::MultisampleState {
            count: self.msaa_sample_count(),
            mask: !0,
            alpha_to_coverage_enabled: false,
        }
    }

    /// Compiles the PBR shader (`pbr_common.wgsl` + `pbr_vs.wgsl`) into a
    /// [`Pipeline`] targeting this context's surface format.
    pub fn create_pbr_pipeline(&self, label: &str) -> Pipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(
                format!("{PBR_COMMON_SOURCE}\n{PBR_VS_SOURCE}").into(),
            ),
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
                    // Normal, emissive and occlusion maps. All three
                    // share binding 1's sampler — they are sampled with
                    // the same UVs and filtering, so four samplers would
                    // be four times the state for identical behaviour.
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
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
            // Opaque geometry: this is what makes draw order stop mattering.
            depth_stencil: Some(opaque_depth_state()),
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        // Same shader, same layouts; only the blend state and depth
        // write differ. Transparent surfaces must not write depth or
        // they would hide each other.
        let transparent_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("{label} (transparent)")),
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
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_TEXTURE_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                // No back-face culling: you can see through a
                // transparent surface to its own far side.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(transparent_depth_state()),
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        Pipeline {
            render_pipeline,
            transparent_pipeline,
            camera_bind_group_layout,
            material_bind_group_layout,
            model_bind_group_layout,
            lights_bind_group_layout,
            neutral: NeutralMaps {
                // Tangent-space "no perturbation": (0, 0, 1) encoded into
                // the 0..1 texture range.
                normal: self.create_texture_from_rgba(
                    "neutral normal",
                    1,
                    1,
                    &[128, 128, 255, 255],
                ),
                emissive: self.create_texture_from_rgba("neutral emissive", 1, 1, &[0, 0, 0, 255]),
                occlusion: self.create_texture_from_rgba(
                    "neutral occlusion",
                    1,
                    1,
                    &[255, 255, 255, 255],
                ),
            },
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
        self.create_material_binding_with(pipeline, texture, material, MaterialMaps::default())
    }

    /// [`GpuContext::create_material_binding`], plus the optional normal,
    /// emissive and occlusion maps.
    ///
    /// Any map left `None` falls back to a neutral 1x1 owned by
    /// `pipeline`, so an absent map has no effect rather than needing a
    /// separate shader path.
    pub fn create_material_binding_with(
        &self,
        pipeline: &Pipeline,
        texture: &Texture,
        material: &MaterialUniform,
        maps: MaterialMaps<'_>,
    ) -> MaterialBinding {
        let buffer = self.create_uniform_buffer("material uniform", material);
        let normal = maps.normal.unwrap_or(&pipeline.neutral.normal);
        let emissive = maps.emissive.unwrap_or(&pipeline.neutral.emissive);
        let occlusion = maps.occlusion.unwrap_or(&pipeline.neutral.occlusion);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material bind group"),
            layout: &pipeline.material_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                // One sampler for all four maps — see the layout's own
                // comment for why.
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&normal.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&emissive.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&occlusion.view),
                },
            ],
        });
        MaterialBinding {
            buffer,
            bind_group,
            // `2` is `AlphaMode::Blend`; see `MaterialUniform::alpha_mode`.
            transparent: material.alpha_mode == 2,
        }
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
            // Sky is a full-screen triangle drawn before geometry; it must
            // never win a depth test and never write depth.
            depth_stencil: Some(overlay_depth_state()),
            multisample: self.scene_multisample_state(),
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
    ///
    /// If [`GpuContext::msaa_sample_count`] is above `1`, this also
    /// allocates a multisampled companion texture; the scene pass renders
    /// into it and resolves into the single-sample one the tonemap pass
    /// samples.
    pub fn create_hdr_target(&self, width: u32, height: u32) -> HdrTarget {
        let device = self.device();

        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        // Single-sample: the resolved image the tonemap pass reads.
        let resolve_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr target (resolve)"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = resolve_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let msaa_view = (self.msaa_sample_count() > 1).then(|| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("hdr target (msaa)"),
                    size,
                    mip_level_count: 1,
                    sample_count: self.msaa_sample_count(),
                    dimension: wgpu::TextureDimension::D2,
                    format: HDR_TEXTURE_FORMAT,
                    // No TEXTURE_BINDING: a multisampled texture is never
                    // sampled directly, only resolved.
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        });

        // Sample count follows the color attachment the scene pass will
        // actually render into: the MSAA texture when multisampling is
        // on, the resolve texture otherwise.
        let depth_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("hdr target (depth)"),
                size,
                mip_level_count: 1,
                sample_count: self.msaa_sample_count(),
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                // No TEXTURE_BINDING: nothing samples scene depth yet. A
                // depth-based outline (see the `post` module docs) would
                // need it, and would also need MSAA depth resolved first.
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        // The post-processing stack that samples this target owns its own
        // sampler ([`GpuContext::create_post_process_stack`]), so none is
        // stored here.
        HdrTarget {
            view,
            msaa_view,
            depth_view,
        }
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
            // Editor overlays (gizmo handles) stay visible through geometry.
            depth_stencil: Some(overlay_depth_state()),
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });

        DebugLinePipeline { render_pipeline }
    }

    /// Renders one frame through a [`crate::RenderGraph`] of three
    /// declared passes — `"shadow"` (writes `"shadow_map"`), `"scene"`
    /// (reads `"shadow_map"`, writes `"hdr_target"`), `"post_process"`
    /// (reads `"hdr_target"`, writes `"swapchain"`) — resolved to that
    /// exact shadow-then-scene-then-post order (each pass depends on the
    /// previous one's output). The `"post_process"` node was the plain
    /// tonemap pass before Stage 5; it now runs the whole
    /// [`crate::PostProcessStack`] (bloom, color grade, toon outline, then
    /// the same ACES tonemap) — slotted in by declaring the same
    /// read/write as the old tonemap pass, no rewrite of this function,
    /// exactly as the render graph was built to allow.
    ///
    /// The `"shadow"` pass (`shadow_pipeline`/`shadow_map`, from the
    /// shadow-casting light's point of view) runs first, over
    /// `shadow_casters` — kept a separate slice from `drawables` so a mesh
    /// culled from the camera's view still casts its shadow into it (the
    /// caller passes the frustum-culled subset as `drawables` and the full
    /// set as `shadow_casters`; passing the same slice for both is fine
    /// when no culling is wanted). The `"scene"`
    /// pass draws the procedural sky (`skybox_pipeline`/`skybox`) across
    /// the whole viewport first, then every [`Drawable`] in `drawables`
    /// on top of it with `pipeline`, `camera`, and `lights` (which
    /// samples `shadow_map` back in the fragment shader; see
    /// `pbr.wgsl`'s `shadow_factor`), optionally followed by
    /// `debug_lines` (`Some((pipeline, vertices))` — e.g. from
    /// `engine_physics::debug_render_lines`, converted to
    /// [`DebugLineVertex`] pairs; `None` or an empty slice draws nothing
    /// extra) drawn on top of everything else, and finally `particles`
    /// (`Some(`[`crate::ParticleFrame`]`)` — the alpha-blended then the
    /// additive billboards, each uploaded to a transient instance buffer;
    /// `None` or empty slices draw nothing) — all into `hdr_target` rather
    /// than the swapchain directly. The `"post_process"` pass runs `post`
    /// (a [`crate::PostProcessStack`]): the bloom chain, then a composite
    /// that grades, outlines, and tonemaps `hdr_target` into the
    /// swapchain's displayable range. All passes share one command buffer
    /// and submit together.
    ///
    /// After the unskinned `drawables`, every [`crate::SkinnedDrawable`]
    /// in `skinned_drawables` is drawn with `skinned_pipeline` (same
    /// camera and lights, GPU vertex skinning in its vertex stage), then
    /// every [`crate::InstancedDrawable`] in `instanced_drawables` with
    /// `instanced_pipeline` (one `draw_indexed(.., 0..N)` per group, model
    /// matrix from an instance buffer), then — when `vegetation` is
    /// `Some((pipeline, wind, drawables))` — those `drawables` again
    /// through the wind-animated [`crate::VegetationPipeline`] with `wind`
    /// bound at `@group(2)`. Empty slices draw nothing extra.
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
        post: &crate::PostProcessStack,
        camera: &CameraBinding,
        lights: &LightsBinding,
        drawables: &[Drawable<'_>],
        shadow_casters: &[Drawable<'_>],
        transparent: &[Drawable<'_>],
        skinned_pipeline: &crate::SkinnedPipeline,
        skinned_drawables: &[crate::SkinnedDrawable<'_>],
        instanced_pipeline: &crate::InstancedPipeline,
        instanced_drawables: &[crate::InstancedDrawable<'_>],
        debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
        sprites: Option<crate::SpriteFrame<'_>>,
        particles: Option<crate::ParticleFrame<'_>>,
        vegetation: Option<(
            &crate::VegetationPipeline,
            &crate::WindBinding,
            &[crate::InstancedDrawable<'_>],
        )>,
        ui: Option<(&crate::UiPipeline, &[crate::UiQuad])>,
    ) -> Result<(), crate::error::RendererError> {
        let Some(surface_texture) = self.acquire_frame() else {
            return Ok(());
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        self.render_scene_to(
            RenderTarget::new(&view),
            pipeline,
            shadow_pipeline,
            shadow_map,
            skybox_pipeline,
            skybox,
            hdr_target,
            post,
            camera,
            lights,
            drawables,
            shadow_casters,
            transparent,
            skinned_pipeline,
            skinned_drawables,
            instanced_pipeline,
            instanced_drawables,
            debug_lines,
            sprites,
            particles,
            vegetation,
            ui,
        )?;

        self.present(surface_texture);
        Ok(())
    }

    /// [`GpuContext::render_scene`], but compositing into `target_view`
    /// instead of acquiring and presenting a swapchain frame.
    ///
    /// Same passes, same order, same everything — this is where the work
    /// actually happens, and `render_scene` is the acquire/present
    /// wrapper around it. Split out for callers that render somewhere
    /// other than the window: the editor's Scene view draws into an
    /// off-screen texture it then displays inside an egui panel.
    ///
    /// `target_view`'s format must match the one the `post` stack was
    /// built for — see
    /// [`GpuContext::create_post_process_stack_for_format`].
    ///
    /// # Errors
    ///
    /// Propagates any [`crate::RenderGraph`] execution error.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors `render_scene`'s parameter list exactly; diverging would defeat the point of the split"
    )]
    pub fn render_scene_to(
        &self,
        target: RenderTarget<'_>,
        pipeline: &Pipeline,
        shadow_pipeline: &ShadowPipeline,
        shadow_map: &ShadowMap,
        skybox_pipeline: &SkyboxPipeline,
        skybox: &SkyboxBinding,
        hdr_target: &HdrTarget,
        post: &crate::PostProcessStack,
        camera: &CameraBinding,
        lights: &LightsBinding,
        drawables: &[Drawable<'_>],
        shadow_casters: &[Drawable<'_>],
        transparent: &[Drawable<'_>],
        skinned_pipeline: &crate::SkinnedPipeline,
        skinned_drawables: &[crate::SkinnedDrawable<'_>],
        instanced_pipeline: &crate::InstancedPipeline,
        instanced_drawables: &[crate::InstancedDrawable<'_>],
        debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
        sprites: Option<crate::SpriteFrame<'_>>,
        particles: Option<crate::ParticleFrame<'_>>,
        vegetation: Option<(
            &crate::VegetationPipeline,
            &crate::WindBinding,
            &[crate::InstancedDrawable<'_>],
        )>,
        ui: Option<(&crate::UiPipeline, &[crate::UiQuad])>,
    ) -> Result<(), crate::error::RendererError> {
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

        // The sprite instance buffer, built out here so it outlives the
        // "scene" pass closure that draws it. `None` for an empty frame:
        // no buffer, no draw, no pipeline switch.
        let sprite_buffer = sprites.filter(|frame| !frame.is_empty()).map(|frame| {
            self.device()
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sprite instances"),
                    contents: bytemuck::cast_slice(frame.instances),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });

        // Same story for the particle instance buffers — built out here so
        // they outlive the "scene" pass closure that draws them. `None`
        // for an empty blend-mode slice.
        let particle_alpha_buffer = particles.and_then(|frame| {
            self.create_particle_instance_buffer("particle instances (alpha)", frame.alpha)
        });
        let particle_additive_buffer = particles.and_then(|frame| {
            self.create_particle_instance_buffer("particle instances (additive)", frame.additive)
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
            for drawable in shadow_casters {
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
            // With MSAA, render into the multisampled texture and resolve
            // into the single-sample one the tonemap pass samples; the
            // multisampled contents themselves are transient (Discard).
            // Without MSAA, render straight into that single-sample view.
            let (scene_view, resolve_target, color_store) = match &hdr_target.msaa_view {
                Some(msaa_view) => (msaa_view, Some(&hdr_target.view), wgpu::StoreOp::Discard),
                None => (&hdr_target.view, None, wgpu::StoreOp::Store),
            };

            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE scene pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    // Into the HDR offscreen target, not the swapchain —
                    // the "tonemap" pass writes the swapchain.
                    view: scene_view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color()),
                        store: color_store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: hdr_target.depth_view(),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
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

            // Skinned geometry: same camera/lights, a different pipeline
            // and `@group(2)` (model matrix + joint matrices). Drawn after
            // the unskinned meshes; no shadow-map contribution yet (the
            // shadow pass has no skinning shader).
            if !skinned_drawables.is_empty() {
                render_pass.set_pipeline(&skinned_pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(3, &lights.bind_group, &[]);
                for skinned in skinned_drawables {
                    render_pass.set_bind_group(1, &skinned.material.bind_group, &[]);
                    render_pass.set_bind_group(2, &skinned.skin.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, skinned.mesh.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(
                        skinned.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(0..skinned.mesh.index_count, 0, 0..1);
                }
            }

            // Instanced geometry: one draw call per mesh/material group,
            // the model matrix coming from the instance-step buffer at
            // vertex slot 1. Same camera/lights; no `@group(2)`. Also no
            // shadow-map contribution yet.
            if !instanced_drawables.is_empty() {
                render_pass.set_pipeline(&instanced_pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(3, &lights.bind_group, &[]);
                for instanced in instanced_drawables {
                    render_pass.set_bind_group(1, &instanced.material.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, instanced.mesh.vertex_buffer.slice(..));
                    render_pass.set_vertex_buffer(1, instanced.instance_buffer.buffer.slice(..));
                    render_pass.set_index_buffer(
                        instanced.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(
                        0..instanced.mesh.index_count,
                        0,
                        0..instanced.instance_count,
                    );
                }
            }

            // Wind-animated vegetation: the instanced path again, but with
            // the vegetation pipeline and a wind uniform bound at
            // `@group(2)` (which the plain instanced pipeline leaves
            // unbound). Same `InstancedDrawable`s, same instance buffers.
            if let Some((vegetation_pipeline, wind, vegetation_drawables)) = vegetation
                && !vegetation_drawables.is_empty()
            {
                render_pass.set_pipeline(&vegetation_pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(2, &wind.bind_group, &[]);
                render_pass.set_bind_group(3, &lights.bind_group, &[]);
                for vegetation_drawable in vegetation_drawables {
                    render_pass.set_bind_group(1, &vegetation_drawable.material.bind_group, &[]);
                    render_pass
                        .set_vertex_buffer(0, vegetation_drawable.mesh.vertex_buffer.slice(..));
                    render_pass
                        .set_vertex_buffer(1, vegetation_drawable.instance_buffer.buffer.slice(..));
                    render_pass.set_index_buffer(
                        vegetation_drawable.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(
                        0..vegetation_drawable.mesh.index_count,
                        0,
                        0..vegetation_drawable.instance_count,
                    );
                }
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

            // Transparent surfaces, after every opaque draw and before
            // particles. The caller has already sorted them back-to-front
            // (see `engine_ecs::extract_and_render`); this pass tests
            // depth but does not write it, so they blend over opaque
            // geometry without occluding each other.
            if !transparent.is_empty() {
                render_pass.set_pipeline(pipeline.transparent());
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(3, &lights.bind_group, &[]);
                for drawable in transparent {
                    render_pass.set_bind_group(1, &drawable.material.bind_group, &[]);
                    render_pass.set_bind_group(2, &drawable.model.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, drawable.mesh.vertex_buffer.slice(..));
                    render_pass.set_index_buffer(
                        drawable.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    render_pass.draw_indexed(0..drawable.mesh.index_count, 0, 0..1);
                }
            }

            // Sprites after the transparent meshes and before particles:
            // alpha-blended quads that test the scene's depth (so a
            // sprite behind a mesh is occluded) without writing it (so
            // they blend over each other in the order the caller sorted
            // them). Inside this pass, so post-processing sees them.
            if let Some(frame) = sprites
                && let Some(buffer) = &sprite_buffer
            {
                render_pass.set_pipeline(&frame.pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(1, &frame.atlas.bind_group, &[]);
                render_pass.set_vertex_buffer(0, frame.quad.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(1, buffer.slice(..));
                render_pass
                    .set_index_buffer(frame.quad.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(
                    0..frame.quad.index_count,
                    0,
                    0..frame.instances.len() as u32,
                );
            }

            // Particles after everything else: transparent billboards, so
            // they must composite over the opaque scene (and the debug
            // overlay). Alpha-blended set first, then additive. Six
            // vertices per instance, generated in the shader. Drawn into
            // this HDR pass so post-processing bloom/tonemap picks them up.
            if let Some(frame) = particles {
                render_pass.set_bind_group(0, &frame.camera.bind_group, &[]);
                if let Some(buffer) = &particle_alpha_buffer {
                    render_pass.set_pipeline(&frame.pipelines.alpha.render_pipeline);
                    render_pass.set_vertex_buffer(0, buffer.buffer.slice(..));
                    render_pass.draw(0..6, 0..buffer.count);
                }
                if let Some(buffer) = &particle_additive_buffer {
                    render_pass.set_pipeline(&frame.pipelines.additive.render_pipeline);
                    render_pass.set_vertex_buffer(0, buffer.buffer.slice(..));
                    render_pass.draw(0..6, 0..buffer.count);
                }
            }
        });

        graph.add_pass("post_process", &["hdr_target"], &["swapchain"], |encoder| {
            // The whole post stack — bloom chain then the grade/outline/
            // tonemap composite — recorded into this node. Internal
            // ping-pong ordering is plain sequential encoder work, the
            // same way the "scene" pass sequences its own sub-draws.
            post.record(encoder, target.view());
        });

        graph.execute(&mut encoder)?;

        // UI last, straight onto the target after post-processing has
        // written it. Inside this encoder so it lands before the caller
        // presents.
        if let Some((ui_pipeline, quads)) = ui {
            self.render_ui(&mut encoder, target.view(), ui_pipeline, quads);
        }

        self.queue().submit(std::iter::once(encoder.finish()));
        Ok(())
    }
}
