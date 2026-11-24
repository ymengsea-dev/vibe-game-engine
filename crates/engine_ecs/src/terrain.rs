//! [`TerrainRenderer`]: a terrain's chunk meshes and tile materials on the
//! GPU, built from an [`engine_renderer::TerrainSurface`].
//!
//! Each tile is cut into chunks; each chunk owns one dynamic [`Mesh`] per
//! detail tier. [`crate::extract_and_render_to`] picks a tier per chunk by
//! camera distance each frame and draws it through the terrain pipeline
//! (and into the shadow map through the normal shadow pipeline). After an
//! edit, [`TerrainRenderer::rewrite_chunks`] re-uploads only the chunks the
//! edit touched, and [`TerrainRenderer::write_splat`] only the paint texels.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU8, Ordering};

use bevy_ecs::component::Component;
use engine_renderer::{
    ChunkKey, DirtyRect, GpuContext, MaterialBinding, Mesh, ModelBinding, ModelUniform, Pipeline,
    RendererError, TerrainLayerImage, TerrainMaterialBinding, TerrainSurface, TileCoord,
    all_chunks, chunk_mesh, chunk_strides, chunk_world_size, skirt_depth,
    terrain_lod_switch_distances,
};

/// One chunk's detail tiers, most detailed first.
pub struct TerrainChunkGpu {
    /// Which chunk.
    pub key: ChunkKey,
    /// One mesh per tier.
    pub tiers: Vec<Mesh>,
    /// Tier drawn last frame, for hysteresis.
    pub lod_tier: AtomicU8,
}

impl TerrainChunkGpu {
    /// The tier drawn last frame.
    pub fn current_tier(&self) -> usize {
        usize::from(self.lod_tier.load(Ordering::Relaxed))
    }
}

/// One tile's material and chunks.
pub struct TerrainTileGpu {
    /// Grid position.
    pub coord: TileCoord,
    /// Paint and layer textures.
    pub material: TerrainMaterialBinding,
    /// Its chunks.
    pub chunks: Vec<TerrainChunkGpu>,
}

/// A terrain ready to draw. See the module docs.
#[derive(Component)]
pub struct TerrainRenderer {
    /// The terrain object's model matrix, rewritten each frame.
    pub model: ModelBinding,
    /// Plain opaque material for the shadow pass.
    pub shadow_material: MaterialBinding,
    /// Distances where chunks switch tier.
    pub switch: [f32; 3],
    /// The tiles.
    pub tiles: Vec<TerrainTileGpu>,
}

fn upload_chunk(
    gpu: &GpuContext,
    surface: &TerrainSurface,
    key: ChunkKey,
) -> Result<TerrainChunkGpu, RendererError> {
    let skirt = skirt_depth(surface, key);
    let tiers = chunk_strides(surface.resolution(), key)
        .into_iter()
        .map(|stride| {
            let (vertices, indices) = chunk_mesh(surface, key, stride, skirt);
            gpu.create_mesh_dynamic("terrain chunk", &vertices, &indices)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TerrainChunkGpu {
        key,
        tiers,
        lod_tier: AtomicU8::new(0),
    })
}

impl TerrainRenderer {
    /// Uploads every chunk of `surface` and a material per tile.
    ///
    /// # Errors
    ///
    /// [`RendererError`] if a chunk mesh can't be created.
    pub fn build(
        gpu: &GpuContext,
        pipeline: &Pipeline,
        surface: &TerrainSurface,
        layers: &[TerrainLayerImage],
        detail_distance: f32,
    ) -> Result<Self, RendererError> {
        let chunks = all_chunks(surface);
        let mut tiles = Vec::with_capacity(surface.tile_count());
        for (coord, tile) in surface.tiles() {
            let tile_chunks = chunks
                .iter()
                .filter(|key| key.tile == *coord)
                .map(|key| upload_chunk(gpu, surface, *key))
                .collect::<Result<Vec<_>, _>>()?;
            tiles.push(TerrainTileGpu {
                coord: *coord,
                material: gpu.create_terrain_material(pipeline, &tile.splat, layers),
                chunks: tile_chunks,
            });
        }
        Ok(Self {
            model: gpu.create_model_binding(
                pipeline,
                &ModelUniform::from_cols(glam::Mat4::IDENTITY.to_cols_array_2d()),
            ),
            shadow_material: gpu.create_plain_material_binding(pipeline),
            switch: terrain_lod_switch_distances(chunk_world_size(surface), detail_distance),
            tiles,
        })
    }

    /// Number of chunks over all tiles.
    pub fn chunk_count(&self) -> usize {
        self.tiles.iter().map(|tile| tile.chunks.len()).sum()
    }

    /// Re-uploads the vertices of `keys` from `surface`. Keys of tiles this
    /// renderer doesn't have are skipped.
    ///
    /// # Errors
    ///
    /// [`RendererError`] from the vertex upload.
    pub fn rewrite_chunks(
        &mut self,
        gpu: &GpuContext,
        surface: &TerrainSurface,
        keys: &BTreeSet<ChunkKey>,
    ) -> Result<(), RendererError> {
        for tile in &mut self.tiles {
            for chunk in &mut tile.chunks {
                if !keys.contains(&chunk.key) {
                    continue;
                }
                let skirt = skirt_depth(surface, chunk.key);
                let strides = chunk_strides(surface.resolution(), chunk.key);
                for (mesh, stride) in chunk.tiers.iter_mut().zip(strides) {
                    let (vertices, _) = chunk_mesh(surface, chunk.key, stride, skirt);
                    gpu.write_mesh_vertices(mesh, &vertices)?;
                }
            }
        }
        Ok(())
    }

    /// Re-uploads the paint texels inside `rect` (global nodes) for every
    /// tile it touches.
    pub fn write_splat(&self, gpu: &GpuContext, surface: &TerrainSurface, rect: DirtyRect) {
        for tile in &self.tiles {
            let (Some(local), Some(source)) = (
                surface.tile_local_rect(tile.coord, rect),
                surface.tile(tile.coord),
            ) else {
                continue;
            };
            gpu.write_terrain_splat(&tile.material, &source.splat, local);
        }
    }

    /// Rebuilds every tile's material for new layers.
    pub fn set_layers(
        &mut self,
        gpu: &GpuContext,
        pipeline: &Pipeline,
        surface: &TerrainSurface,
        layers: &[TerrainLayerImage],
    ) {
        for tile in &mut self.tiles {
            if let Some(source) = surface.tile(tile.coord) {
                tile.material = gpu.create_terrain_material(pipeline, &source.splat, layers);
            }
        }
    }

    /// Changes how far away chunks switch to less detail.
    pub fn set_detail_distance(&mut self, surface: &TerrainSurface, detail_distance: f32) {
        self.switch = terrain_lod_switch_distances(chunk_world_size(surface), detail_distance);
    }
}

/// The tier to draw for a chunk `distance` away, remembered for next
/// frame's hysteresis.
pub(crate) fn pick_chunk_tier(chunk: &TerrainChunkGpu, switch: &[f32; 3], distance: f32) -> usize {
    let tier = engine_renderer::terrain_chunk_tier(
        switch,
        chunk.tiers.len(),
        distance,
        chunk.current_tier(),
    )
    .min(chunk.tiers.len().saturating_sub(1));
    chunk.lod_tier.store(tier as u8, Ordering::Relaxed);
    tier
}
