//! The screen-space pass that draws game UI: panels, images, and text.
//!
//! One pipeline handles all three. Every quad is an instance carrying a
//! pixel rect, an atlas sub-rect, and a tint; a solid fill points at a
//! white texel, so there is no branching and no second pipeline.
//!
//! ## Drawn after post-processing
//!
//! Tonemapping and bloom exist to make a *scene* look right. Running them
//! over a HUD makes it muddy and stops white text being white, so this
//! pass targets the final surface directly, after post has composited.
//!
//! ## One atlas
//!
//! The pass binds a single texture. [`crate::GlyphAtlas`]'s glyph sheet
//! occupies it, with a white texel reserved for solid fills. A game that
//! wants image widgets supplies its own atlas containing both.

use wgpu::util::DeviceExt;

use crate::gpu::GpuContext;
use crate::texture::Texture;

const UI_SHADER_SOURCE: &str = include_str!("shaders/ui.wgsl");

/// One quad to draw, in screen pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiQuad {
    /// `x`, `y`, `width`, `height` in pixels from the window's top-left.
    pub rect: [f32; 4],
    /// Atlas sub-rect: `min_u`, `min_v`, `max_u`, `max_v`.
    pub uv: [f32; 4],
    /// Linear RGBA tint, multiplied with the sampled texel.
    pub tint: [f32; 4],
}

impl UiQuad {
    /// The instance-buffer layout matching `ui.wgsl`'s `Instance`.
    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<UiQuad>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 16,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: 32,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

/// Reciprocal screen size, for the pixels-to-clip-space conversion.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ScreenUniform {
    /// `1 / width`, `1 / height`.
    pub inv_size: [f32; 2],
    /// Padding only — keeps the struct 16-byte aligned.
    pub _padding: [f32; 2],
}

impl ScreenUniform {
    /// The uniform for a `width` x `height` window. A zero dimension is
    /// treated as one, so a minimized window cannot divide by zero.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            inv_size: [1.0 / width.max(1) as f32, 1.0 / height.max(1) as f32],
            _padding: [0.0; 2],
        }
    }
}

/// The UI pipeline plus its screen uniform and atlas binding.
pub struct UiPipeline {
    render_pipeline: wgpu::RenderPipeline,
    screen_buffer: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    /// Where in the atlas a fully opaque white texel lives, for solid
    /// fills.
    white_uv: [f32; 4],
}

impl UiPipeline {
    /// Updates the screen size this pass maps pixels against. Call on
    /// resize.
    pub fn resize(&self, gpu: &GpuContext, width: u32, height: u32) {
        gpu.write_uniform_buffer(&self.screen_buffer, &ScreenUniform::new(width, height));
    }

    /// The atlas UV of the white texel used for untextured fills.
    pub fn white_uv(&self) -> [f32; 4] {
        self.white_uv
    }
}

impl GpuContext {
    /// Builds the UI pass against `atlas`.
    ///
    /// `white_uv` must name a fully opaque white texel within `atlas`;
    /// solid-colour quads sample it so one pipeline covers fills, images
    /// and glyphs.
    pub fn create_ui_pipeline(
        &self,
        label: &str,
        atlas: &Texture,
        white_uv: [f32; 4],
    ) -> UiPipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(UI_SHADER_SOURCE.into()),
        });

        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui screen bind group layout"),
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
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui atlas bind group layout"),
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
            ],
        });

        let size = self.config();
        let screen_buffer = self.create_uniform_buffer(
            "ui screen uniform",
            &ScreenUniform::new(size.width, size.height),
        );
        let screen_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui screen bind group"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buffer.as_entire_binding(),
            }],
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui atlas bind group"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.sampler),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui pipeline layout"),
            bind_group_layouts: &[Some(&screen_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(UiQuad::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // Straight to the surface: this runs after post, so it
                // matches the swapchain's format, not the HDR target's.
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.config().format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            // No depth at all: UI is painted in list order, over
            // everything, and the surface has no depth attachment anyway.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        UiPipeline {
            render_pipeline,
            screen_buffer,
            screen_bind_group,
            atlas_bind_group,
            white_uv,
        }
    }

    /// Draws `quads` over `target`, blending, in list order.
    ///
    /// A no-op for an empty slice — a game with no UI pays nothing.
    pub fn render_ui(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        pipeline: &UiPipeline,
        quads: &[UiQuad],
    ) {
        if quads.is_empty() {
            return;
        }

        let instances = self
            .device()
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ui instances"),
                contents: bytemuck::cast_slice(quads),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ui pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Load, never clear: the scene is already there.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&pipeline.render_pipeline);
        pass.set_bind_group(0, &pipeline.screen_bind_group, &[]);
        pass.set_bind_group(1, &pipeline.atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, instances.slice(..));
        pass.draw(0..6, 0..quads.len() as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_uniform_inverts_the_size() {
        let uniform = ScreenUniform::new(800, 400);
        assert!((uniform.inv_size[0] - 1.0 / 800.0).abs() < 1e-9);
        assert!((uniform.inv_size[1] - 1.0 / 400.0).abs() < 1e-9);
    }

    #[test]
    fn screen_uniform_survives_a_zero_dimension() {
        // A minimized window reports zero; dividing by it would produce
        // infinities that poison every vertex.
        let uniform = ScreenUniform::new(0, 0);
        assert!(uniform.inv_size[0].is_finite());
        assert!(uniform.inv_size[1].is_finite());
    }

    #[test]
    fn uniform_and_quad_are_16_byte_aligned() {
        assert_eq!(std::mem::size_of::<ScreenUniform>() % 16, 0);
        assert_eq!(std::mem::size_of::<UiQuad>() % 16, 0);
    }

    #[test]
    fn quad_layout_stride_matches_the_struct() {
        assert_eq!(
            UiQuad::layout().array_stride as usize,
            std::mem::size_of::<UiQuad>(),
        );
    }
}
