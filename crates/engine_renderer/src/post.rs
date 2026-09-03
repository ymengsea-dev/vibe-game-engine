//! Post-processing stack: the passes that run on the HDR scene image after
//! the main color pass and before the swapchain gets its final pixels.
//!
//! Three effects, one owner ([`PostProcessStack`]):
//!
//! - **Bloom** — a bright-pass (`bloom_prefilter.wgsl`) into a
//!   half-resolution target, then a few ping-ponged separable-Gaussian
//!   blur iterations (`bloom_blur.wgsl`), added back into the image in
//!   the composite pass. Deliberately the "easy" bloom (fixed iteration
//!   count, single half-res level) rather than a dual-filter mip pyramid —
//!   right-sized for this engine, same stance the `render_graph` module
//!   takes; a pyramid is a later upgrade.
//! - **Color grade** — exposure, an approximate white balance, a color
//!   filter multiply, contrast around 18% grey, and saturation. All in
//!   linear HDR, folded into the composite shader.
//! - **Toon outline** — a screen-space Sobel edge on scene luminance,
//!   mixed in as a flat ink color after tonemapping. A depth/normal-buffer
//!   silhouette outline needs the scene pass to gain a depth attachment
//!   first and is tracked as future work.
//!
//! The composite pass (`post_composite.wgsl`) **replaces** the old
//! standalone tonemap pass: it applies the same ACES filmic curve last, so
//! [`PostSettings::IDENTITY`] (no bloom, neutral grade, no outline)
//! reproduces the previous output exactly.
//!
//! Like [`crate::HdrTarget`], the half-resolution bloom targets are sized
//! to the window, so [`GpuContext::create_post_process_stack`] must be
//! called again (rebuilding the stack) on every resize. Per-frame tweaks
//! that don't change sizes go through [`PostProcessStack::set_settings`]
//! instead, which only rewrites uniform buffers.

use crate::gpu::GpuContext;
use crate::pipeline::HdrTarget;

/// Multiplies HDR radiance before the tonemap curve — a scene-wide
/// brightness knob. `1.0` leaves radiance as computed by the lighting
/// pipeline. (Moved here from the former `hdr` module when the tonemap
/// pass was folded into the post-processing composite pass.)
pub const DEFAULT_EXPOSURE: f32 = 1.0;

/// How many horizontal+vertical blur iterations the bloom chain runs. Each
/// iteration widens the effective kernel well past its literal 9-tap
/// footprint; five is a good spread/cost balance at half resolution.
const BLOOM_ITERATIONS: u32 = 5;

/// Bloom tuning. Bloom is skipped entirely (the blur targets cleared to
/// black) when `intensity` is `0.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BloomSettings {
    /// Max-channel brightness above which a pixel starts contributing to
    /// bloom.
    pub threshold: f32,
    /// Width of the smooth ramp below `threshold` over which contribution
    /// fades in, so pixels near the threshold don't pop.
    pub soft_knee: f32,
    /// Scales the blurred bloom before it's added back into the image.
    /// `0.0` disables the bloom passes.
    pub intensity: f32,
}

impl BloomSettings {
    /// Bloom off.
    pub const DISABLED: Self = Self {
        threshold: 1.0,
        soft_knee: 0.5,
        intensity: 0.0,
    };
    /// A subtle default glow on highlights above `1.0`.
    pub const DEFAULT: Self = Self {
        threshold: 1.0,
        soft_knee: 0.5,
        intensity: 0.04,
    };
}

impl Default for BloomSettings {
    /// [`BloomSettings::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Color grade tuning, applied in linear HDR in the composite pass. Every
/// field's neutral value leaves the image unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorGrade {
    /// Approximate white balance: `> 0.0` warms (more red, less blue),
    /// `< 0.0` cools. Neutral at `0.0`. Not a calibrated CCT model.
    pub temperature: f32,
    /// Green/magenta push: `> 0.0` greener, `< 0.0` more magenta. Neutral
    /// at `0.0`.
    pub tint: f32,
    /// Contrast multiplier around 18% grey. Neutral at `1.0`.
    pub contrast: f32,
    /// Saturation: `1.0` neutral, `0.0` greyscale, `> 1.0` more saturated.
    pub saturation: f32,
    /// Linear RGB multiplier applied to the whole image. `[1.0; 3]`
    /// neutral.
    pub color_filter: [f32; 3],
}

impl ColorGrade {
    /// Every field at its neutral value — the grade is a no-op.
    pub const NEUTRAL: Self = Self {
        temperature: 0.0,
        tint: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        color_filter: [1.0; 3],
    };
}

impl Default for ColorGrade {
    /// [`ColorGrade::NEUTRAL`].
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// Toon-outline tuning. Disabled when `intensity` is `0.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineSettings {
    /// The ink color edges are drawn in (linear RGB).
    pub color: [f32; 3],
    /// Sobel gradient magnitude above which a pixel counts as an edge.
    pub threshold: f32,
    /// Scales the Sobel sample spacing — effectively the outline width, in
    /// multiples of one screen texel.
    pub thickness: f32,
    /// How strongly detected edges are mixed toward `color`. `0.0`
    /// disables the outline.
    pub intensity: f32,
}

impl OutlineSettings {
    /// Outline off.
    pub const DISABLED: Self = Self {
        color: [0.0; 3],
        threshold: 0.6,
        thickness: 1.0,
        intensity: 0.0,
    };
}

impl Default for OutlineSettings {
    /// [`OutlineSettings::DISABLED`].
    fn default() -> Self {
        Self::DISABLED
    }
}

/// The full post-processing configuration: what [`PostProcessStack`]
/// renders each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostSettings {
    /// Scene-wide exposure multiplier applied before tonemapping.
    pub exposure: f32,
    /// Bloom tuning.
    pub bloom: BloomSettings,
    /// Color grade tuning.
    pub color_grade: ColorGrade,
    /// Toon-outline tuning.
    pub outline: OutlineSettings,
}

impl PostSettings {
    /// No bloom, neutral grade, no outline, [`DEFAULT_EXPOSURE`]. The
    /// composite pass with these settings produces output identical to the
    /// plain ACES tonemap pass that preceded this module.
    pub const IDENTITY: Self = Self {
        exposure: DEFAULT_EXPOSURE,
        bloom: BloomSettings::DISABLED,
        color_grade: ColorGrade::NEUTRAL,
        outline: OutlineSettings::DISABLED,
    };
    /// A tasteful starting point: subtle bloom, neutral grade, no outline.
    pub const DEFAULT: Self = Self {
        exposure: DEFAULT_EXPOSURE,
        bloom: BloomSettings::DEFAULT,
        color_grade: ColorGrade::NEUTRAL,
        outline: OutlineSettings::DISABLED,
    };

    fn prefilter_uniform(&self) -> PrefilterUniform {
        PrefilterUniform {
            threshold: self.bloom.threshold,
            soft_knee: self.bloom.soft_knee.max(1e-4),
            _padding: [0.0; 2],
        }
    }

    fn composite_uniform(&self, texel_size: [f32; 2]) -> CompositeUniform {
        CompositeUniform {
            exposure: self.exposure,
            bloom_intensity: self.bloom.intensity,
            grade_temperature: self.color_grade.temperature,
            grade_tint: self.color_grade.tint,
            grade_contrast: self.color_grade.contrast,
            grade_saturation: self.color_grade.saturation,
            outline_threshold: self.outline.threshold,
            outline_thickness: self.outline.thickness,
            grade_color_filter: self.color_grade.color_filter,
            outline_intensity: self.outline.intensity,
            outline_color: self.outline.color,
            _padding0: 0.0,
            texel_size,
            _padding1: [0.0; 2],
        }
    }
}

impl Default for PostSettings {
    /// [`PostSettings::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// GPU-layout bright-pass input. Same `#[repr(C)]` + `Pod`/`Zeroable`
/// pattern as [`crate::CameraUniform`]; 16 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PrefilterUniform {
    /// See [`BloomSettings::threshold`].
    pub threshold: f32,
    /// See [`BloomSettings::soft_knee`] — clamped to at least `1e-4`.
    pub soft_knee: f32,
    /// Padding to a 16-byte multiple (WGSL uniform struct-size rule).
    pub _padding: [f32; 2],
}

/// GPU-layout separable-blur input: one texel step along the axis being
/// blurred this pass. 16 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BlurUniform {
    /// `(texel_x, 0.0)` for the horizontal pass, `(0.0, texel_y)` for the
    /// vertical one — in half-resolution UV units.
    pub direction: [f32; 2],
    /// Padding to a 16-byte multiple.
    pub _padding: [f32; 2],
}

/// GPU-layout composite-pass input: exposure, bloom strength, the whole
/// color grade, the outline parameters, and the screen texel size the
/// Sobel outline needs. 80 bytes; every member's offset satisfies the WGSL
/// uniform alignment rules (see the matching struct in
/// `post_composite.wgsl`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompositeUniform {
    /// See [`PostSettings::exposure`].
    pub exposure: f32,
    /// See [`BloomSettings::intensity`].
    pub bloom_intensity: f32,
    /// See [`ColorGrade::temperature`].
    pub grade_temperature: f32,
    /// See [`ColorGrade::tint`].
    pub grade_tint: f32,
    /// See [`ColorGrade::contrast`].
    pub grade_contrast: f32,
    /// See [`ColorGrade::saturation`].
    pub grade_saturation: f32,
    /// See [`OutlineSettings::threshold`].
    pub outline_threshold: f32,
    /// See [`OutlineSettings::thickness`].
    pub outline_thickness: f32,
    /// See [`ColorGrade::color_filter`]. `vec3` in WGSL — the following
    /// `f32` shares its 16-byte slot.
    pub grade_color_filter: [f32; 3],
    /// See [`OutlineSettings::intensity`].
    pub outline_intensity: f32,
    /// See [`OutlineSettings::color`].
    pub outline_color: [f32; 3],
    /// Padding filling `outline_color`'s 16-byte slot.
    pub _padding0: f32,
    /// `(1.0 / width, 1.0 / height)` of the swapchain — the Sobel sample
    /// spacing at `thickness == 1.0`.
    pub texel_size: [f32; 2],
    /// Padding to a 16-byte multiple.
    pub _padding1: [f32; 2],
}

/// Owns every GPU resource the post-processing stack needs: the three
/// pipelines, the two half-resolution ping-pong bloom targets, their bind
/// groups, and the uniform buffers [`PostProcessStack::set_settings`]
/// rewrites.
///
/// Built by [`GpuContext::create_post_process_stack`] and rebuilt on every
/// window resize (the bloom targets are window-sized and the bind groups
/// point at both them and the [`HdrTarget`]'s view, none of which can be
/// repointed in place).
pub struct PostProcessStack {
    prefilter_pipeline: wgpu::RenderPipeline,
    blur_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,

    bloom_a_view: wgpu::TextureView,
    bloom_b_view: wgpu::TextureView,

    prefilter_bind_group: wgpu::BindGroup,
    blur_h_bind_group: wgpu::BindGroup,
    blur_v_bind_group: wgpu::BindGroup,
    composite_bind_group: wgpu::BindGroup,

    prefilter_buffer: wgpu::Buffer,
    composite_buffer: wgpu::Buffer,

    output_texel_size: [f32; 2],
    settings: PostSettings,
}

impl PostProcessStack {
    /// The current settings — e.g. so a resize can rebuild the stack
    /// carrying the same configuration forward.
    pub fn settings(&self) -> PostSettings {
        self.settings
    }

    /// Rewrites the bright-pass and composite uniform buffers from
    /// `settings` without rebuilding any GPU resources. Use this for
    /// per-frame or interactive tuning; use
    /// [`GpuContext::create_post_process_stack`] again only when the window
    /// size changes.
    pub fn set_settings(&mut self, gpu: &GpuContext, settings: PostSettings) {
        self.settings = settings;
        gpu.write_uniform_buffer(&self.prefilter_buffer, &settings.prefilter_uniform());
        gpu.write_uniform_buffer(
            &self.composite_buffer,
            &settings.composite_uniform(self.output_texel_size),
        );
    }

    /// Records the whole stack into `encoder`: the bloom bright-pass and
    /// blur iterations (skipped, with the bloom target cleared to black,
    /// when bloom intensity is `0.0`), then the composite pass that writes
    /// `swapchain_view`.
    ///
    /// Called from the `"post_process"` node of
    /// [`GpuContext::render_scene`]'s render graph — which reads
    /// `"hdr_target"`, so the scene pass has already resolved into the
    /// [`HdrTarget`] this stack's bind groups sample.
    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        swapchain_view: &wgpu::TextureView,
    ) {
        if self.settings.bloom.intensity > 0.0 {
            self.record_bloom(encoder);
        } else {
            // Composite still samples the bloom target unconditionally;
            // give it a defined all-black value this frame.
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE bloom clear (disabled)"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.bloom_a_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let mut composite_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("VGE post composite pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: swapchain_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Fully overwritten by the full-screen triangle;
                    // `Clear` is just a defined starting state.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        composite_pass.set_pipeline(&self.composite_pipeline);
        composite_pass.set_bind_group(0, &self.composite_bind_group, &[]);
        composite_pass.draw(0..3, 0..1);
    }

    /// The bright-pass (HDR -> `bloom_a`) followed by [`BLOOM_ITERATIONS`]
    /// H/V blur iterations, each ping-ponging `bloom_a` <-> `bloom_b` and
    /// ending back in `bloom_a` (what the composite pass samples).
    fn record_bloom(&self, encoder: &mut wgpu::CommandEncoder) {
        {
            let mut prefilter_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE bloom prefilter pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.bloom_a_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            prefilter_pass.set_pipeline(&self.prefilter_pipeline);
            prefilter_pass.set_bind_group(0, &self.prefilter_bind_group, &[]);
            prefilter_pass.draw(0..3, 0..1);
        }

        for _ in 0..BLOOM_ITERATIONS {
            // Horizontal: bloom_a -> bloom_b.
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("VGE bloom blur H pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.bloom_b_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blur_pipeline);
                pass.set_bind_group(0, &self.blur_h_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            // Vertical: bloom_b -> bloom_a.
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("VGE bloom blur V pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.bloom_a_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.blur_pipeline);
                pass.set_bind_group(0, &self.blur_v_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

const BLOOM_PREFILTER_SOURCE: &str = include_str!("shaders/bloom_prefilter.wgsl");
const BLOOM_BLUR_SOURCE: &str = include_str!("shaders/bloom_blur.wgsl");
const POST_COMPOSITE_SOURCE: &str = include_str!("shaders/post_composite.wgsl");

/// A `texture_2d<f32>` + filtering sampler + uniform buffer bind group
/// layout — shared shape for the bright-pass and blur pipelines (they
/// differ only in which uniform struct binding 2 holds).
fn sampled_texture_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
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
    })
}

/// A full-screen-triangle post pipeline: no vertex buffer, no depth, no
/// culling, single-sample (it reads the already-resolved HDR target). Same
/// shape the old tonemap pipeline used.
fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(target_format.into())],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl GpuContext {
    /// Builds a [`PostProcessStack`] for a `width` x `height` swapchain,
    /// reading `hdr_target` (the resolved HDR scene image) and writing the
    /// swapchain. `settings` seeds the uniform buffers;
    /// [`PostProcessStack::set_settings`] changes them afterward.
    ///
    /// Must be called again whenever the window resizes — the
    /// half-resolution bloom targets are window-sized and every bind group
    /// points at resources (them, and `hdr_target`'s view) that can't be
    /// repointed in place, exactly like [`GpuContext::create_hdr_target`]
    /// and its old tonemap binding.
    pub fn create_post_process_stack(
        &self,
        hdr_target: &HdrTarget,
        width: u32,
        height: u32,
        settings: PostSettings,
    ) -> PostProcessStack {
        let device = self.device();

        let bloom_width = (width / 2).max(1);
        let bloom_height = (height / 2).max(1);
        let bloom_extent = wgpu::Extent3d {
            width: bloom_width,
            height: bloom_height,
            depth_or_array_layers: 1,
        };

        let make_bloom_target = |label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: bloom_extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: crate::pipeline::HDR_TEXTURE_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let bloom_a_view = make_bloom_target("bloom target A");
        let bloom_b_view = make_bloom_target("bloom target B");

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- bright-pass pipeline ---
        let prefilter_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bloom prefilter shader"),
            source: wgpu::ShaderSource::Wgsl(BLOOM_PREFILTER_SOURCE.into()),
        });
        let prefilter_layout = sampled_texture_layout(device, "bloom prefilter bind group layout");
        let prefilter_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("bloom prefilter layout"),
                bind_group_layouts: &[Some(&prefilter_layout)],
                immediate_size: 0,
            });
        let prefilter_pipeline = fullscreen_pipeline(
            device,
            "bloom prefilter",
            &prefilter_shader,
            &prefilter_pipeline_layout,
            crate::pipeline::HDR_TEXTURE_FORMAT,
        );

        // --- blur pipeline ---
        let blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bloom blur shader"),
            source: wgpu::ShaderSource::Wgsl(BLOOM_BLUR_SOURCE.into()),
        });
        let blur_layout = sampled_texture_layout(device, "bloom blur bind group layout");
        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bloom blur layout"),
            bind_group_layouts: &[Some(&blur_layout)],
            immediate_size: 0,
        });
        let blur_pipeline = fullscreen_pipeline(
            device,
            "bloom blur",
            &blur_shader,
            &blur_pipeline_layout,
            crate::pipeline::HDR_TEXTURE_FORMAT,
        );

        // --- composite pipeline ---
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post composite shader"),
            source: wgpu::ShaderSource::Wgsl(POST_COMPOSITE_SOURCE.into()),
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post composite bind group layout"),
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
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
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("post composite layout"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });
        let composite_pipeline = fullscreen_pipeline(
            device,
            "post composite",
            &composite_shader,
            &composite_pipeline_layout,
            self.config().format,
        );

        // --- uniform buffers ---
        let output_texel_size = [1.0 / width.max(1) as f32, 1.0 / height.max(1) as f32];
        let prefilter_buffer =
            self.create_uniform_buffer("bloom prefilter uniform", &settings.prefilter_uniform());
        let composite_buffer = self.create_uniform_buffer(
            "post composite uniform",
            &settings.composite_uniform(output_texel_size),
        );
        let blur_h_buffer = self.create_uniform_buffer(
            "bloom blur H uniform",
            &BlurUniform {
                direction: [1.0 / bloom_width as f32, 0.0],
                _padding: [0.0; 2],
            },
        );
        let blur_v_buffer = self.create_uniform_buffer(
            "bloom blur V uniform",
            &BlurUniform {
                direction: [0.0, 1.0 / bloom_height as f32],
                _padding: [0.0; 2],
            },
        );

        // --- bind groups ---
        let prefilter_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom prefilter bind group"),
            layout: &prefilter_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr_target.color_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: prefilter_buffer.as_entire_binding(),
                },
            ],
        });
        let blur_h_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom blur H bind group"),
            layout: &blur_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&bloom_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: blur_h_buffer.as_entire_binding(),
                },
            ],
        });
        let blur_v_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom blur V bind group"),
            layout: &blur_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&bloom_b_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: blur_v_buffer.as_entire_binding(),
                },
            ],
        });
        let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post composite bind group"),
            layout: &composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr_target.color_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&bloom_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: composite_buffer.as_entire_binding(),
                },
            ],
        });

        PostProcessStack {
            prefilter_pipeline,
            blur_pipeline,
            composite_pipeline,
            bloom_a_view,
            bloom_b_view,
            prefilter_bind_group,
            blur_h_bind_group,
            blur_v_bind_group,
            composite_bind_group,
            prefilter_buffer,
            composite_buffer,
            output_texel_size,
            settings,
        }
    }
}

/// CPU mirrors of the per-pixel math in the post shaders, kept only so the
/// unit tests below can pin that math down without a GPU. Not used by the
/// stack itself (the shaders are the real implementation), hence
/// `#[cfg(test)]`.
#[cfg(test)]
mod shader_mirror {
    /// Soft-knee bright-pass response — mirrors `bloom_prefilter.wgsl`'s
    /// `soft_knee_response`. Given a pixel whose brightest channel is
    /// `brightness`, returns the `[0, 1]` fraction of its color that
    /// contributes to bloom: `0.0` well below `threshold - soft_knee`,
    /// approaching `(brightness - threshold) / brightness` well above
    /// `threshold`, a smooth ramp between.
    pub(super) fn soft_knee_response(brightness: f32, threshold: f32, soft_knee: f32) -> f32 {
        let knee = soft_knee.max(1e-4);
        let mut soft = (brightness - threshold + knee).clamp(0.0, 2.0 * knee);
        soft = soft * soft / (4.0 * knee);
        soft.max(brightness - threshold).max(0.0) / brightness.max(1e-4)
    }

    /// The five unique weights of the symmetric 9-tap Gaussian in
    /// `bloom_blur.wgsl` — centre tap then the four one-sided taps.
    pub(super) const GAUSSIAN_9TAP_WEIGHTS: [f32; 5] = [
        0.227_027,
        0.194_594_6,
        0.121_621_6,
        0.054_054_1,
        0.016_216_2,
    ];

    /// Total energy of the 9-tap kernel: `weights[0] + 2 * (rest summed)`.
    pub(super) fn gaussian_9tap_sum(weights: [f32; 5]) -> f32 {
        weights[0] + 2.0 * (weights[1] + weights[2] + weights[3] + weights[4])
    }

    /// Rec. 709 luma of a linear RGB triple — mirrors the composite
    /// shader's `luma`.
    pub(super) fn luma(rgb: [f32; 3]) -> f32 {
        0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
    }

    /// Contrast around 18% grey — mirrors the composite shader's
    /// `apply_contrast`, per channel. `amount == 1.0` is the identity.
    pub(super) fn apply_contrast(channel: f32, amount: f32) -> f32 {
        ((channel - 0.18) * amount + 0.18).max(0.0)
    }

    /// Saturation toward/away from luma — mirrors the composite shader's
    /// `apply_saturation`. `amount == 1.0` identity, `0.0` greyscale.
    pub(super) fn apply_saturation(rgb: [f32; 3], amount: f32) -> [f32; 3] {
        let l = luma(rgb);
        [
            (l + (rgb[0] - l) * amount).max(0.0),
            (l + (rgb[1] - l) * amount).max(0.0),
            (l + (rgb[2] - l) * amount).max(0.0),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::shader_mirror::{
        GAUSSIAN_9TAP_WEIGHTS, apply_contrast, apply_saturation, gaussian_9tap_sum, luma,
        soft_knee_response,
    };
    use super::*;

    #[test]
    fn identity_settings_have_neutral_grade_and_no_bloom_or_outline() {
        let s = PostSettings::IDENTITY;
        assert_eq!(s.exposure, DEFAULT_EXPOSURE);
        assert_eq!(s.bloom.intensity, 0.0);
        assert_eq!(s.outline.intensity, 0.0);
        assert_eq!(s.color_grade, ColorGrade::NEUTRAL);
    }

    #[test]
    fn default_settings_enable_only_subtle_bloom() {
        let s = PostSettings::default();
        assert!(s.bloom.intensity > 0.0);
        assert_eq!(s.color_grade, ColorGrade::NEUTRAL);
        assert_eq!(s.outline.intensity, 0.0);
    }

    #[test]
    fn prefilter_uniform_clamps_soft_knee_away_from_zero() {
        let mut s = PostSettings::IDENTITY;
        s.bloom.soft_knee = 0.0;
        assert!(s.prefilter_uniform().soft_knee >= 1e-4);
    }

    #[test]
    fn composite_uniform_carries_texel_size_and_grade() {
        let mut s = PostSettings::IDENTITY;
        s.color_grade.contrast = 1.5;
        let u = s.composite_uniform([0.001, 0.002]);
        assert_eq!(u.texel_size, [0.001, 0.002]);
        assert_eq!(u.grade_contrast, 1.5);
        assert_eq!(u.bloom_intensity, 0.0);
    }

    #[test]
    fn soft_knee_response_is_zero_below_the_knee() {
        // brightness 0.1, threshold 1.0, knee 0.5: 0.1 - 1.0 + 0.5 < 0.
        assert_eq!(soft_knee_response(0.1, 1.0, 0.5), 0.0);
    }

    #[test]
    fn soft_knee_response_approaches_linear_excess_when_bright() {
        // brightness 3.0, threshold 1.0: excess 2.0 dominates the knee
        // term, so response ~ 2.0 / 3.0.
        let r = soft_knee_response(3.0, 1.0, 0.5);
        assert!((r - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn soft_knee_response_ramps_between_knee_and_threshold() {
        let below = soft_knee_response(0.7, 1.0, 0.5);
        let mid = soft_knee_response(0.9, 1.0, 0.5);
        let at = soft_knee_response(1.0, 1.0, 0.5);
        assert!(below >= 0.0);
        assert!(mid > below);
        assert!(at > mid);
    }

    #[test]
    fn gaussian_kernel_conserves_energy() {
        assert!((gaussian_9tap_sum(GAUSSIAN_9TAP_WEIGHTS) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn contrast_of_one_is_identity() {
        for c in [0.0_f32, 0.18, 0.5, 1.0, 4.0] {
            assert!((apply_contrast(c, 1.0) - c).abs() < 1e-6);
        }
    }

    #[test]
    fn contrast_pushes_away_from_mid_grey() {
        assert!(apply_contrast(0.5, 2.0) > 0.5);
        assert!(apply_contrast(0.05, 2.0) < 0.05);
    }

    #[test]
    fn saturation_of_one_is_identity() {
        let rgb = [0.2_f32, 0.6, 0.9];
        let out = apply_saturation(rgb, 1.0);
        for i in 0..3 {
            assert!((out[i] - rgb[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn saturation_of_zero_collapses_to_luma() {
        let rgb = [0.2_f32, 0.6, 0.9];
        let l = luma(rgb);
        let out = apply_saturation(rgb, 0.0);
        for channel in out {
            assert!((channel - l).abs() < 1e-6);
        }
    }

    #[test]
    fn uniform_sizes_are_16_byte_multiples() {
        assert_eq!(size_of::<PrefilterUniform>() % 16, 0);
        assert_eq!(size_of::<BlurUniform>() % 16, 0);
        assert_eq!(size_of::<CompositeUniform>() % 16, 0);
    }

    #[test]
    fn composite_uniform_is_80_bytes() {
        // Matches the WGSL struct layout in post_composite.wgsl.
        assert_eq!(size_of::<CompositeUniform>(), 80);
    }

    #[test]
    fn uniforms_round_trip_through_bytes() {
        let u = PostSettings::default().composite_uniform([0.01, 0.02]);
        let bytes = bytemuck::bytes_of(&u);
        let back: CompositeUniform = *bytemuck::from_bytes(bytes);
        assert_eq!(u, back);
    }
}
