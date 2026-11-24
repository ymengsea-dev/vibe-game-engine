//! GPU side of terrain: the paint-layer pipeline and each tile's material
//! (its two paint textures, the layer texture array and the layer
//! settings).
//!
//! [`TerrainPipeline`] is `pbr_shared.wgsl` + `terrain_fs.wgsl` +
//! `pbr_vs.wgsl`, built by [`GpuContext::create_pbr_pipeline`] alongside
//! the mesh pipeline so the two share camera, model and light layouts.
//! Chunk geometry is an ordinary [`Mesh`] (see `terrain_chunks`), so the
//! shadow pass draws terrain through the normal shadow pipeline with
//! [`GpuContext::create_plain_material_binding`].

use crate::gpu::GpuContext;
use crate::mesh::{Mesh, Vertex};
use crate::pipeline::{ModelBinding, PBR_SHARED_SOURCE, Pipeline};
use crate::terrain_surface::{DirtyRect, MAX_TERRAIN_LAYERS, SplatMap};
use crate::terrain_textures::{BuiltinLayer, builtin_layer_rgba};

const TERRAIN_FS_SOURCE: &str = include_str!("shaders/terrain_fs.wgsl");
const PBR_VS_SOURCE: &str = include_str!("shaders/pbr_vs.wgsl");

/// Edge length every layer texture is resized to.
pub const TERRAIN_LAYER_TEXTURE_SIZE: u32 = 256;

/// The compiled terrain pipeline and its material layout.
pub struct TerrainPipeline {
    pub(crate) render_pipeline: wgpu::RenderPipeline,
    material_layout: wgpu::BindGroupLayout,
    layer_sampler: wgpu::Sampler,
    splat_sampler: wgpu::Sampler,
}

/// GPU layout of the terrain material uniform; 176 bytes, matching
/// `TerrainUniform` in `terrain_fs.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainUniform {
    /// Linear RGB tint per layer; `w` unused.
    pub tints: [[f32; 4]; MAX_TERRAIN_LAYERS],
    /// World units per texture copy, four layers per row.
    pub tiling: [[f32; 4]; 2],
    /// Layers in use.
    pub layer_count: u32,
    /// Paint texels per side.
    pub splat_resolution: f32,
    /// Surface roughness.
    pub roughness: f32,
    /// Padding to a 16-byte multiple.
    pub _padding: f32,
}

/// One paint layer ready for upload: a square sRGB image plus its tint and
/// tiling.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLayerImage {
    /// [`TERRAIN_LAYER_TEXTURE_SIZE`]² RGBA8 sRGB pixels.
    pub rgba: Vec<u8>,
    /// Linear RGB tint.
    pub tint: [f32; 3],
    /// World units per texture copy.
    pub tiling: f32,
}

impl TerrainLayerImage {
    /// A built-in ground texture.
    pub fn builtin(layer: BuiltinLayer, tint: [f32; 3], tiling: f32) -> Self {
        Self {
            rgba: builtin_layer_rgba(layer, TERRAIN_LAYER_TEXTURE_SIZE),
            tint,
            tiling,
        }
    }

    /// A project image, resized to [`TERRAIN_LAYER_TEXTURE_SIZE`]².
    ///
    /// # Errors
    ///
    /// [`crate::RendererError::ImageDecode`] if the image can't be read.
    pub fn from_image_bytes(
        bytes: &[u8],
        tint: [f32; 3],
        tiling: f32,
    ) -> Result<Self, crate::RendererError> {
        let image = crate::decode_rgba8(bytes)?;
        let size = TERRAIN_LAYER_TEXTURE_SIZE;
        let resized = if image.width() == size && image.height() == size {
            image
        } else {
            image::imageops::resize(&image, size, size, image::imageops::FilterType::Triangle)
        };
        Ok(Self {
            rgba: resized.into_raw(),
            tint,
            tiling,
        })
    }
}

/// A tile's terrain material: paint textures, layer textures and settings,
/// bound at `@group(1)` of [`TerrainPipeline`].
pub struct TerrainMaterialBinding {
    splat: [wgpu::Texture; 2],
    resolution: u32,
    /// The settings uniform. Kept for updates.
    pub buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
}

impl TerrainMaterialBinding {
    /// Paint texels per side.
    pub fn resolution(&self) -> u32 {
        self.resolution
    }
}

/// One terrain chunk tier to draw.
#[derive(Clone, Copy)]
pub struct TerrainDrawable<'a> {
    /// The terrain object's model matrix.
    pub model: &'a ModelBinding,
    /// Its tile's material.
    pub material: &'a TerrainMaterialBinding,
    /// The chunk tier's geometry.
    pub mesh: &'a Mesh,
}

/// The uniform for `layers`, padded to eight.
pub fn terrain_uniform(layers: &[TerrainLayerImage], splat_resolution: u32) -> TerrainUniform {
    let mut uniform = TerrainUniform {
        tints: [[1.0; 4]; MAX_TERRAIN_LAYERS],
        tiling: [[4.0; 4]; 2],
        layer_count: layers.len().clamp(1, MAX_TERRAIN_LAYERS) as u32,
        splat_resolution: splat_resolution as f32,
        roughness: 0.9,
        _padding: 0.0,
    };
    for (index, layer) in layers.iter().take(MAX_TERRAIN_LAYERS).enumerate() {
        let tint = layer
            .tint
            .map(|c| if c.is_finite() { c.max(0.0) } else { 1.0 });
        uniform.tints[index] = [tint[0], tint[1], tint[2], 1.0];
        uniform.tiling[index / 4][index % 4] = if layer.tiling.is_finite() && layer.tiling > 0.0 {
            layer.tiling
        } else {
            4.0
        };
    }
    uniform
}

/// Halves an RGBA8 image with a 2x2 box filter.
fn half_size(rgba: &[u8], size: u32) -> Vec<u8> {
    let half = (size / 2).max(1);
    let src = size as usize;
    let mut out = Vec::with_capacity((half * half * 4) as usize);
    for y in 0..half as usize {
        for x in 0..half as usize {
            for channel in 0..4 {
                let at = |dx: usize, dy: usize| {
                    let (sx, sy) = ((x * 2 + dx).min(src - 1), (y * 2 + dy).min(src - 1));
                    u32::from(rgba[(sy * src + sx) * 4 + channel])
                };
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    out
}

impl GpuContext {
    /// Compiles the terrain shader against the mesh pipeline's camera,
    /// model and light layouts.
    pub(crate) fn create_terrain_pipeline(
        &self,
        camera_layout: &wgpu::BindGroupLayout,
        model_layout: &wgpu::BindGroupLayout,
        lights_layout: &wgpu::BindGroupLayout,
    ) -> TerrainPipeline {
        let device = self.device();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain shader"),
            source: wgpu::ShaderSource::Wgsl(
                format!("{PBR_SHARED_SOURCE}\n{TERRAIN_FS_SOURCE}\n{PBR_VS_SOURCE}").into(),
            ),
        });
        let texture = |binding, dimension| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: dimension,
                multisampled: false,
            },
            count: None,
        };
        let sampler = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain material bind group layout"),
            entries: &[
                texture(0, wgpu::TextureViewDimension::D2),
                texture(1, wgpu::TextureViewDimension::D2),
                texture(2, wgpu::TextureViewDimension::D2Array),
                sampler(3),
                sampler(4),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain pipeline layout"),
            bind_group_layouts: &[
                Some(camera_layout),
                Some(&material_layout),
                Some(model_layout),
                Some(lights_layout),
            ],
            immediate_size: 0,
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain pipeline"),
            layout: Some(&layout),
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
                targets: &[Some(crate::pipeline::HDR_TEXTURE_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(crate::pipeline::opaque_depth_state()),
            multisample: self.scene_multisample_state(),
            multiview_mask: None,
            cache: None,
        });
        let layer_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terrain layer sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let splat_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terrain paint sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        TerrainPipeline {
            render_pipeline,
            material_layout,
            layer_sampler,
            splat_sampler,
        }
    }

    /// An opaque white material for drawing terrain chunks into the shadow
    /// map, which needs a mesh material binding.
    pub fn create_plain_material_binding(&self, pipeline: &Pipeline) -> crate::MaterialBinding {
        self.create_material_binding(
            pipeline,
            pipeline.white_texture(),
            &crate::MaterialUniform::from(crate::Material::DEFAULT),
        )
    }

    /// Uploads a tile's paint and `layers` as a terrain material. Layers
    /// past the eighth are ignored; an empty list draws plain white.
    pub fn create_terrain_material(
        &self,
        pipeline: &Pipeline,
        splat: &SplatMap,
        layers: &[TerrainLayerImage],
    ) -> TerrainMaterialBinding {
        let terrain = pipeline.terrain();
        let device = self.device();
        let resolution = splat.resolution().max(1);
        let splat_texture = |label: &str, map: usize| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: resolution,
                    height: resolution,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.write_texture_region(
                &texture,
                &splat.rgba(map),
                resolution,
                0,
                0,
                resolution,
                resolution,
            );
            texture
        };
        let splat_textures = [
            splat_texture("terrain paint 0", 0),
            splat_texture("terrain paint 1", 1),
        ];

        let size = TERRAIN_LAYER_TEXTURE_SIZE;
        let mip_count = size.ilog2() + 1;
        let layer_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terrain layers"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: MAX_TERRAIN_LAYERS as u32,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let blank = vec![255u8; (size * size * 4) as usize];
        for layer in 0..MAX_TERRAIN_LAYERS {
            let mut level = layers
                .get(layer)
                .filter(|image| image.rgba.len() == blank.len())
                .map_or_else(|| blank.clone(), |image| image.rgba.clone());
            let mut level_size = size;
            for mip in 0..mip_count {
                self.queue().write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &layer_texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer as u32,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &level,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(level_size * 4),
                        rows_per_image: Some(level_size),
                    },
                    wgpu::Extent3d {
                        width: level_size,
                        height: level_size,
                        depth_or_array_layers: 1,
                    },
                );
                if level_size > 1 {
                    level = half_size(&level, level_size);
                    level_size /= 2;
                }
            }
        }

        let buffer =
            self.create_uniform_buffer("terrain uniform", &terrain_uniform(layers, resolution));
        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let splat_views = [view(&splat_textures[0]), view(&splat_textures[1])];
        let layer_view = layer_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain material bind group"),
            layout: &terrain.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&splat_views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&splat_views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&terrain.layer_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&terrain.splat_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        TerrainMaterialBinding {
            splat: splat_textures,
            resolution,
            buffer,
            bind_group,
        }
    }

    /// Re-uploads the paint texels of `splat` inside `rect` (tile-local
    /// rows and columns, inclusive). A resolution mismatch is ignored.
    pub fn write_terrain_splat(
        &self,
        material: &TerrainMaterialBinding,
        splat: &SplatMap,
        rect: DirtyRect,
    ) {
        let resolution = material.resolution;
        if splat.resolution() != resolution {
            return;
        }
        let last = i64::from(resolution) - 1;
        let (c0, c1) = (
            rect.col_min.clamp(0, last) as u32,
            rect.col_max.clamp(0, last) as u32,
        );
        let (r0, r1) = (
            rect.row_min.clamp(0, last) as u32,
            rect.row_max.clamp(0, last) as u32,
        );
        if c0 > c1 || r0 > r1 {
            return;
        }
        let (width, height) = (c1 - c0 + 1, r1 - r0 + 1);
        for (map, texture) in material.splat.iter().enumerate() {
            let full = splat.rgba(map);
            let mut region = Vec::with_capacity((width * height * 4) as usize);
            for row in r0..=r1 {
                let start = ((row * resolution + c0) * 4) as usize;
                region.extend_from_slice(&full[start..start + (width * 4) as usize]);
            }
            self.write_texture_region(texture, &region, width, c0, r0, width, height);
        }
    }

    /// Writes tightly packed RGBA8 `rows` into `texture` at `(x, y)`.
    #[allow(
        clippy::too_many_arguments,
        reason = "a texture region is naturally its origin and extent"
    )]
    fn write_texture_region(
        &self,
        texture: &wgpu::Texture,
        rows: &[u8],
        row_texels: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) {
        self.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rows,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_texels * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_uniform_is_176_bytes_and_pads_layers() {
        assert_eq!(std::mem::size_of::<TerrainUniform>(), 176);
        let layers = [
            TerrainLayerImage {
                rgba: Vec::new(),
                tint: [0.5, f32::NAN, 2.0],
                tiling: 8.0,
            },
            TerrainLayerImage {
                rgba: Vec::new(),
                tint: [1.0; 3],
                tiling: -1.0,
            },
        ];
        let uniform = terrain_uniform(&layers, 129);
        assert_eq!(uniform.layer_count, 2);
        assert_eq!(uniform.tints[0], [0.5, 1.0, 2.0, 1.0]);
        assert_eq!(uniform.tiling[0], [8.0, 4.0, 4.0, 4.0]);
        assert_eq!(uniform.splat_resolution, 129.0);
        assert_eq!(terrain_uniform(&[], 2).layer_count, 1);
    }

    #[test]
    fn half_size_averages_two_by_two_blocks() {
        let image = [
            0, 0, 0, 255, 255, 255, 255, 255, //
            255, 255, 255, 255, 0, 0, 0, 255,
        ];
        assert_eq!(half_size(&image, 2), vec![128, 128, 128, 255]);
    }

    #[test]
    fn layer_images_are_resized_to_the_layer_size() {
        let png = crate::encode_rgba8_png(4, 4, &[200; 64]).unwrap();
        let image = TerrainLayerImage::from_image_bytes(&png, [1.0; 3], 2.0).unwrap();
        assert_eq!(
            image.rgba.len(),
            (TERRAIN_LAYER_TEXTURE_SIZE.pow(2) * 4) as usize
        );
        assert!(TerrainLayerImage::from_image_bytes(b"nope", [1.0; 3], 2.0).is_err());
    }
}
