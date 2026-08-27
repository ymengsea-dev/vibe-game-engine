//! 2D sprite batch rendering: an instanced, alpha-blended textured-quad
//! pipeline, plus texture atlas support so many differently-shaped
//! sprites can share one texture (and one draw call).
//!
//! Deliberately standalone from [`crate::Pipeline`]'s 3D scene pass — no
//! shadows, skybox, or HDR/tonemap chain, and (unlike every other pipeline
//! in this crate so far) real alpha blending, since transparent sprites
//! are the common case. [`GpuContext::render_sprites`] draws directly into
//! the swapchain, one full self-contained frame, the same shape as
//! [`GpuContext::render_clear`].
//!
//! The camera driving this is the same [`crate::Camera`]/[`CameraUniform`]
//! the 3D pipeline uses — set [`crate::Projection::Orthographic`] and it
//! works unmodified; no separate 2D camera type.

use std::collections::HashMap;

use wgpu::util::DeviceExt;

use crate::camera::CameraUniform;
use crate::error::RendererError;
use crate::gpu::GpuContext;
use crate::mesh::{Mesh, Vertex};
use crate::pipeline::CameraBinding;
use crate::texture::Texture;

const SPRITE_SHADER_SOURCE: &str = include_str!("shaders/sprite.wgsl");

/// A normalized (`[0, 1]`) UV rectangle into an atlas texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvRect {
    /// Top-left corner, `[u, v]`.
    pub min: [f32; 2],
    /// Bottom-right corner, `[u, v]`.
    pub max: [f32; 2],
}

/// A pixel-space rectangle within an atlas image — the input unit for
/// [`AtlasLayout::add_region`], converted to a normalized [`UvRect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge, in pixels.
    pub x: u32,
    /// Top edge, in pixels.
    pub y: u32,
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
}

/// A texture atlas's pixel-rect-to-normalized-UV mapping — pure data, no
/// GPU dependency. Split out from [`TextureAtlas`] the same way
/// [`crate::decode_rgba8`] is split from [`Texture`]: the mapping math is
/// what can fail on bad input, and is worth unit-testing without a real
/// device.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AtlasLayout {
    width: u32,
    height: u32,
    regions: HashMap<String, UvRect>,
}

impl AtlasLayout {
    /// A layout for an atlas image of `width` x `height` pixels, with no
    /// regions registered yet.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::InvalidAtlasSize`] if either dimension is
    /// zero.
    pub fn new(width: u32, height: u32) -> Result<Self, RendererError> {
        if width == 0 || height == 0 {
            return Err(RendererError::InvalidAtlasSize { width, height });
        }
        Ok(Self {
            width,
            height,
            regions: HashMap::new(),
        })
    }

    /// This atlas image's width, in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// This atlas image's height, in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Registers `rect` under `name`, converted to a normalized [`UvRect`].
    /// A second call with the same `name` overwrites the first.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::AtlasRegionOutOfBounds`] if `rect` has
    /// zero width/height, or doesn't fit within this atlas's bounds.
    pub fn add_region(
        &mut self,
        name: impl Into<String>,
        rect: PixelRect,
    ) -> Result<(), RendererError> {
        let name = name.into();
        let fits = rect.width > 0
            && rect.height > 0
            && rect
                .x
                .checked_add(rect.width)
                .is_some_and(|right| right <= self.width)
            && rect
                .y
                .checked_add(rect.height)
                .is_some_and(|bottom| bottom <= self.height);
        if !fits {
            return Err(RendererError::AtlasRegionOutOfBounds {
                name,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                atlas_width: self.width,
                atlas_height: self.height,
            });
        }

        let atlas_width = self.width as f32;
        let atlas_height = self.height as f32;
        let uv = UvRect {
            min: [rect.x as f32 / atlas_width, rect.y as f32 / atlas_height],
            max: [
                (rect.x + rect.width) as f32 / atlas_width,
                (rect.y + rect.height) as f32 / atlas_height,
            ],
        };
        self.regions.insert(name, uv);
        Ok(())
    }

    /// Slices this atlas into a uniform `columns` x `rows` grid,
    /// registering each cell as `"{col}_{row}"` (row-major, top-left
    /// origin, zero-indexed) — the common sprite-sheet layout (walk
    /// cycles, tile sets).
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::InvalidAtlasGrid`] if `columns`/`rows` is
    /// zero, or doesn't evenly divide this atlas's width/height — rejected
    /// rather than truncated, since a fractional-pixel cell would bleed
    /// into its neighbor at sample time.
    pub fn add_grid(&mut self, columns: u32, rows: u32) -> Result<(), RendererError> {
        let invalid = columns == 0
            || rows == 0
            || !self.width.is_multiple_of(columns)
            || !self.height.is_multiple_of(rows);
        if invalid {
            return Err(RendererError::InvalidAtlasGrid {
                columns,
                rows,
                atlas_width: self.width,
                atlas_height: self.height,
            });
        }

        let cell_width = self.width / columns;
        let cell_height = self.height / rows;
        for row in 0..rows {
            for col in 0..columns {
                self.add_region(
                    format!("{col}_{row}"),
                    PixelRect {
                        x: col * cell_width,
                        y: row * cell_height,
                        width: cell_width,
                        height: cell_height,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// The normalized UV rectangle registered under `name`, or `None` if
    /// no region by that name was ever added.
    pub fn uv_rect(&self, name: &str) -> Option<UvRect> {
        self.regions.get(name).copied()
    }
}

/// A GPU-resident atlas texture plus its [`AtlasLayout`]. Plain
/// composition — building `texture` goes through the existing
/// [`GpuContext::create_texture_from_bytes`]/
/// [`GpuContext::create_texture_from_rgba`], no new upload path needed.
pub struct TextureAtlas {
    /// The atlas image itself.
    pub texture: Texture,
    /// This atlas's region mapping.
    pub layout: AtlasLayout,
}

impl TextureAtlas {
    /// Wraps an already-uploaded `texture` with its `layout`.
    pub fn new(texture: Texture, layout: AtlasLayout) -> Self {
        Self { texture, layout }
    }
}

/// White, fully opaque — [`SpriteInstance::new`]'s default tint (i.e. "no
/// tint", the sampled texture color unchanged).
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// One sprite's per-instance draw data: where it is, how big, which
/// region of the atlas it samples, and its tint.
///
/// `#[repr(C)]` + [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make this
/// safely castable to bytes for GPU upload, the same pattern as
/// [`crate::DebugLineVertex`]. Drawn via [`SpriteBatch`] and
/// [`GpuContext::render_sprites`], one instanced draw call per batch.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpriteInstance {
    /// World-space anchor position (the quad's center). `z` doesn't
    /// affect depth testing (this pipeline has no depth buffer, same as
    /// [`crate::Pipeline`]) — it only matters through the camera's own
    /// view/projection, e.g. as a manual draw-order hint under an
    /// orthographic camera looking down `-Z`.
    pub position: [f32; 3],
    /// World-space width/height.
    pub size: [f32; 2],
    /// In-plane rotation, radians, counter-clockwise.
    pub rotation: f32,
    /// Top-left UV of the atlas region this sprite samples.
    pub uv_min: [f32; 2],
    /// Bottom-right UV of the atlas region this sprite samples.
    pub uv_max: [f32; 2],
    /// Linear RGBA tint, multiplied into the sampled texture color.
    pub color: [f32; 4],
}

impl SpriteInstance {
    /// A sprite at `position`, `size` world units across, sampling `uv`,
    /// with no rotation and no tint (white).
    pub fn new(position: [f32; 3], size: [f32; 2], uv: UvRect) -> Self {
        Self {
            position,
            size,
            rotation: 0.0,
            uv_min: uv.min,
            uv_max: uv.max,
            color: WHITE,
        }
    }

    const ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        3 => Float32x3, 4 => Float32x2, 5 => Float32,
        6 => Float32x2, 7 => Float32x2, 8 => Float32x4,
    ];

    /// This instance format's wgpu buffer layout (`step_mode: Instance`,
    /// attribute locations `3..=8` — continuing after [`Vertex`]'s `0..=2`,
    /// since [`GpuContext::render_sprites`] binds both buffers together).
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<SpriteInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

/// A frame's worth of sprites to draw, accumulated CPU-side and uploaded/
/// drawn in one instanced call by [`GpuContext::render_sprites`] — the
/// "batch" in "sprite batch renderer".
///
/// Draw order is push order (painter's algorithm, no depth buffer) — sort
/// before pushing if draw order matters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpriteBatch {
    instances: Vec<SpriteInstance>,
}

impl SpriteBatch {
    /// An empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `instance`, to be drawn after everything already pushed.
    pub fn push(&mut self, instance: SpriteInstance) {
        self.instances.push(instance);
    }

    /// Removes every pushed instance, ready for reuse next frame.
    pub fn clear(&mut self) {
        self.instances.clear();
    }

    /// This batch's instances, in push (draw) order.
    pub fn instances(&self) -> &[SpriteInstance] {
        &self.instances
    }

    /// Whether this batch has no instances.
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    /// How many instances this batch holds.
    pub fn len(&self) -> usize {
        self.instances.len()
    }
}

/// A compiled instanced-textured-quad render pipeline: `sprite.wgsl`,
/// expecting the camera uniform (`@group(0)`, the same shape as
/// [`crate::Pipeline`]'s) and an atlas texture+sampler (`@group(1)`).
/// Alpha-blended, no depth buffer, targets the swapchain's own format
/// directly (see the module docs for why this is standalone from
/// [`crate::Pipeline`]).
pub struct SpritePipeline {
    render_pipeline: wgpu::RenderPipeline,
    camera_bind_group_layout: wgpu::BindGroupLayout,
    atlas_bind_group_layout: wgpu::BindGroupLayout,
}

/// A [`TextureAtlas`]'s texture/sampler, bound to a [`SpritePipeline`]'s
/// shader at `@group(1)`. No uniform buffer (unlike
/// [`crate::MaterialBinding`]) — an atlas has no per-frame-changing data.
pub struct SpriteAtlasBinding {
    bind_group: wgpu::BindGroup,
}

impl GpuContext {
    /// Compiles `sprite.wgsl` into a [`SpritePipeline`] targeting this
    /// context's surface format, with alpha blending enabled — the one
    /// pipeline in this crate that needs real sprite transparency rather
    /// than opaque draw-order compositing.
    pub fn create_sprite_pipeline(&self, label: &str) -> SpritePipeline {
        let device = self.device();

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&format!("{label} shader")),
            source: wgpu::ShaderSource::Wgsl(SPRITE_SHADER_SOURCE.into()),
        });

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sprite camera bind group layout"),
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

        let atlas_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sprite atlas bind group layout"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&format!("{label} layout")),
            bind_group_layouts: &[
                Some(&camera_bind_group_layout),
                Some(&atlas_bind_group_layout),
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
                buffers: &[Some(Vertex::layout()), Some(SpriteInstance::layout())],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
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
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        SpritePipeline {
            render_pipeline,
            camera_bind_group_layout,
            atlas_bind_group_layout,
        }
    }

    /// Creates a uniform buffer + bind group exposing `uniform` to
    /// `pipeline`'s shader — the sprite-pipeline counterpart of
    /// [`GpuContext::create_camera_binding`], reusing [`CameraBinding`]
    /// itself (against this pipeline's own, differently-shaped camera bind
    /// group layout) rather than a duplicate type.
    pub fn create_sprite_camera_binding(
        &self,
        pipeline: &SpritePipeline,
        uniform: &CameraUniform,
    ) -> CameraBinding {
        let buffer = self.create_uniform_buffer("sprite camera uniform", uniform);
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite camera bind group"),
            layout: &pipeline.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        CameraBinding { buffer, bind_group }
    }

    /// Creates the bind group exposing `atlas`'s texture/sampler to
    /// `pipeline`'s shader.
    pub fn create_sprite_atlas_binding(
        &self,
        pipeline: &SpritePipeline,
        atlas: &TextureAtlas,
    ) -> SpriteAtlasBinding {
        let bind_group = self.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite atlas bind group"),
            layout: &pipeline.atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.texture.sampler),
                },
            ],
        });
        SpriteAtlasBinding { bind_group }
    }

    /// Renders one full frame of `batch`: acquires the current surface
    /// texture, clears it, draws every instance in `batch` against `quad`
    /// (typically built from [`crate::quad`] via
    /// [`GpuContext::create_mesh`]) in a single instanced draw call, and
    /// presents. Standalone — doesn't touch [`crate::Pipeline`]'s HDR
    /// target or the swapchain any other pass might also be writing.
    ///
    /// An empty `batch` still clears and presents (the "nothing to draw
    /// yet" case, not an error) — no instance buffer is allocated for it.
    ///
    /// Skip/error handling for surface texture acquisition matches
    /// [`GpuContext::render_clear`] — see its docs for the full list of
    /// transient conditions treated as "skip this frame".
    ///
    /// # Errors
    ///
    /// This currently never returns `Err` (all failure modes are
    /// recoverable skips), but returns `Result` for the same reason
    /// [`GpuContext::render_clear`] does.
    pub fn render_sprites(
        &self,
        pipeline: &SpritePipeline,
        quad: &Mesh,
        camera: &CameraBinding,
        atlas: &SpriteAtlasBinding,
        batch: &SpriteBatch,
    ) -> Result<(), RendererError> {
        let Some(surface_texture) = self.acquire_frame() else {
            return Ok(());
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VGE sprite frame encoder"),
            });

        // Built here (not inside the pass below) so it outlives the pass
        // that borrows it — same reasoning as `render_scene`'s debug-line
        // vertex buffer. `None` for an empty batch: a zero-length instance
        // buffer is legal but pointless to allocate.
        let instance_buffer = (!batch.is_empty()).then(|| {
            self.device()
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("sprite instances"),
                    contents: bytemuck::cast_slice(batch.instances()),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE sprite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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

            if let Some(instance_buffer) = &instance_buffer {
                render_pass.set_pipeline(&pipeline.render_pipeline);
                render_pass.set_bind_group(0, &camera.bind_group, &[]);
                render_pass.set_bind_group(1, &atlas.bind_group, &[]);
                render_pass.set_vertex_buffer(0, quad.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(1, instance_buffer.slice(..));
                render_pass
                    .set_index_buffer(quad.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                render_pass.draw_indexed(0..quad.index_count, 0, 0..batch.len() as u32);
            }
        }

        self.queue().submit(std::iter::once(encoder.finish()));
        self.present(surface_texture);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_layout_new_rejects_zero_width() {
        assert!(matches!(
            AtlasLayout::new(0, 100),
            Err(RendererError::InvalidAtlasSize {
                width: 0,
                height: 100
            })
        ));
    }

    #[test]
    fn atlas_layout_new_rejects_zero_height() {
        assert!(AtlasLayout::new(100, 0).is_err());
    }

    #[test]
    fn atlas_layout_new_accepts_nonzero_dimensions() {
        let layout = AtlasLayout::new(100, 200).unwrap();
        assert_eq!(layout.width(), 100);
        assert_eq!(layout.height(), 200);
    }

    #[test]
    fn add_region_computes_normalized_uv() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        layout
            .add_region(
                "hero",
                PixelRect {
                    x: 10,
                    y: 10,
                    width: 20,
                    height: 20,
                },
            )
            .unwrap();
        let uv = layout.uv_rect("hero").unwrap();
        assert!((uv.min[0] - 0.1).abs() < 1e-6);
        assert!((uv.min[1] - 0.1).abs() < 1e-6);
        assert!((uv.max[0] - 0.3).abs() < 1e-6);
        assert!((uv.max[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn add_region_covering_the_whole_atlas_is_0_0_to_1_1() {
        let mut layout = AtlasLayout::new(64, 32).unwrap();
        layout
            .add_region(
                "all",
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 32,
                },
            )
            .unwrap();
        let uv = layout.uv_rect("all").unwrap();
        assert_eq!(uv.min, [0.0, 0.0]);
        assert_eq!(uv.max, [1.0, 1.0]);
    }

    #[test]
    fn add_region_rejects_rect_extending_past_the_right_edge() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        let err = layout
            .add_region(
                "oob",
                PixelRect {
                    x: 90,
                    y: 0,
                    width: 20,
                    height: 10,
                },
            )
            .unwrap_err();
        assert!(matches!(err, RendererError::AtlasRegionOutOfBounds { .. }));
    }

    #[test]
    fn add_region_rejects_rect_extending_past_the_bottom_edge() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(
            layout
                .add_region(
                    "oob",
                    PixelRect {
                        x: 0,
                        y: 90,
                        width: 10,
                        height: 20,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn add_region_rejects_zero_width() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(
            layout
                .add_region(
                    "empty",
                    PixelRect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 10,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn add_region_rejects_x_plus_width_overflow() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(
            layout
                .add_region(
                    "overflow",
                    PixelRect {
                        x: u32::MAX,
                        y: 0,
                        width: u32::MAX,
                        height: 10,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn add_region_overwrites_same_name() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        layout
            .add_region(
                "a",
                PixelRect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
            )
            .unwrap();
        layout
            .add_region(
                "a",
                PixelRect {
                    x: 50,
                    y: 50,
                    width: 10,
                    height: 10,
                },
            )
            .unwrap();
        let uv = layout.uv_rect("a").unwrap();
        assert!((uv.min[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn uv_rect_returns_none_for_unknown_name() {
        let layout = AtlasLayout::new(100, 100).unwrap();
        assert_eq!(layout.uv_rect("nope"), None);
    }

    #[test]
    fn add_grid_registers_every_cell_with_correct_uv() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        layout.add_grid(2, 2).unwrap();

        let top_left = layout.uv_rect("0_0").unwrap();
        assert_eq!(top_left.min, [0.0, 0.0]);
        assert_eq!(top_left.max, [0.5, 0.5]);

        let bottom_right = layout.uv_rect("1_1").unwrap();
        assert_eq!(bottom_right.min, [0.5, 0.5]);
        assert_eq!(bottom_right.max, [1.0, 1.0]);
    }

    #[test]
    fn add_grid_rejects_zero_columns() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(layout.add_grid(0, 2).is_err());
    }

    #[test]
    fn add_grid_rejects_zero_rows() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(layout.add_grid(2, 0).is_err());
    }

    #[test]
    fn add_grid_rejects_non_divisible_dimensions() {
        let mut layout = AtlasLayout::new(100, 100).unwrap();
        assert!(matches!(
            layout.add_grid(3, 2),
            Err(RendererError::InvalidAtlasGrid { .. })
        ));
    }

    #[test]
    fn sprite_instance_new_has_no_rotation_and_white_tint() {
        let instance = SpriteInstance::new(
            [1.0, 2.0, 3.0],
            [10.0, 20.0],
            UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        );
        assert_eq!(instance.position, [1.0, 2.0, 3.0]);
        assert_eq!(instance.size, [10.0, 20.0]);
        assert_eq!(instance.rotation, 0.0);
        assert_eq!(instance.color, WHITE);
    }

    #[test]
    fn sprite_instance_layout_stride_matches_struct_size() {
        assert_eq!(
            SpriteInstance::layout().array_stride,
            size_of::<SpriteInstance>() as wgpu::BufferAddress
        );
    }

    #[test]
    fn sprite_instance_layout_has_six_attributes() {
        assert_eq!(SpriteInstance::layout().attributes.len(), 6);
    }

    #[test]
    fn sprite_instance_layout_is_instance_stepped() {
        assert_eq!(
            SpriteInstance::layout().step_mode,
            wgpu::VertexStepMode::Instance
        );
    }

    #[test]
    fn sprite_batch_starts_empty() {
        let batch = SpriteBatch::new();
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
        assert!(batch.instances().is_empty());
    }

    #[test]
    fn sprite_batch_push_appends_in_order() {
        let mut batch = SpriteBatch::new();
        let a = SpriteInstance::new(
            [0.0, 0.0, 0.0],
            [1.0, 1.0],
            UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        );
        let b = SpriteInstance::new(
            [1.0, 0.0, 0.0],
            [1.0, 1.0],
            UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        );
        batch.push(a);
        batch.push(b);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.instances(), &[a, b]);
    }

    #[test]
    fn sprite_batch_clear_empties_it() {
        let mut batch = SpriteBatch::new();
        batch.push(SpriteInstance::new(
            [0.0, 0.0, 0.0],
            [1.0, 1.0],
            UvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
        ));
        batch.clear();
        assert!(batch.is_empty());
    }
}
