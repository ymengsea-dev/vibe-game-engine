//! Studio's live copy of each terrain in the open scene.
//!
//! [`LiveTerrains`] holds every terrain object's
//! [`engine_renderer::TerrainSurface`], loaded from its tile files. Brush
//! strokes edit it directly and queue [`TerrainGpuUpdate`]s, which
//! [`sync_gpu`] turns into chunk and paint re-uploads on the object's
//! [`engine_ecs::TerrainRenderer`]. The tile files are written only when a
//! stroke ends, so disk and the live copy agree between strokes.
//!
//! The Scene View builds terrain renderers from here, not from the scene
//! resolver, so sculpting shows up while the pointer moves.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use engine_ecs::TerrainRenderer;
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{
    ChunkKey, DirtyRect, GpuContext, Heightmap, Pipeline, TerrainSurface, TileCoord, dirty_chunks,
};
use engine_scene::{AssetRef, SceneTerrain, TerrainData, TerrainToolStateData};
use glam::{Mat4, Vec3};

use crate::assets::AssetIndex;

/// One terrain's live data.
pub struct LiveTerrain {
    /// Heights and paint of every tile.
    pub surface: TerrainSurface,
    /// Each tile's file.
    pub tile_paths: BTreeMap<TileCoord, PathBuf>,
    /// Scatter controls read from the first tile.
    pub tools: TerrainToolStateData,
    /// The scene data this copy was built from.
    pub data: TerrainData,
}

impl LiveTerrain {
    /// Each tile's heights with the matrix from tile-local to terrain-local
    /// space: tile `(x, z)` sits `(x * size, z * size)` from the origin.
    pub fn tile_heightmaps(&self) -> Vec<(Heightmap, Mat4)> {
        let size = self.surface.size();
        self.surface
            .tiles()
            .map(|(coord, tile)| {
                let offset = Vec3::new(coord.0 as f32 * size, 0.0, coord.1 as f32 * size);
                (tile.heights.clone(), Mat4::from_translation(offset))
            })
            .collect()
    }
}

/// What changed on the GPU side of one terrain.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TerrainGpuUpdate {
    /// Chunks whose vertices must be re-uploaded.
    pub chunks: BTreeSet<ChunkKey>,
    /// Global nodes whose paint must be re-uploaded.
    pub splat: Option<DirtyRect>,
}

/// How far a terrain's GPU side is out of date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stale {
    /// Build it from scratch.
    Rebuild,
    /// Only the layer textures or settings changed.
    Layers,
}

/// Every terrain in the open scene. See the module docs.
#[derive(Default)]
pub struct LiveTerrains {
    entries: HashMap<Entity, LiveTerrain>,
    /// Data that failed to load, so it isn't retried every frame.
    failed: HashMap<Entity, TerrainData>,
    stale: HashMap<Entity, Stale>,
    updates: HashMap<Entity, TerrainGpuUpdate>,
}

/// The file an asset id names, inside the project.
pub fn asset_path(
    index: &AssetIndex,
    project: &engine_project::Project,
    asset: &AssetRef,
) -> Option<PathBuf> {
    let id = crate::assets::parse_asset_id(&asset.id)?;
    let relative = index.path_for(id)?;
    project.resolve(&Path::new("assets").join(relative)).ok()
}

/// Loads every tile of `data` from disk.
fn load(
    data: &TerrainData,
    resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>,
) -> Result<LiveTerrain, String> {
    let mut tiles: Vec<_> = data.tiles.iter().collect();
    tiles.sort_by_key(|tile| (tile.z, tile.x));
    let mut surface: Option<TerrainSurface> = None;
    let mut tile_paths = BTreeMap::new();
    let mut tools = None;
    for tile in tiles {
        let path = resolve(&tile.asset).ok_or_else(|| {
            format!(
                "Terrain tile ({}, {}) is missing from the project",
                tile.x, tile.z
            )
        })?;
        let (loaded, tile_tools) = engine_scene::load_tile_file(&path)
            .map_err(|error| format!("Could not load {}: {error}", path.display()))?;
        let target = match &mut surface {
            Some(surface) => surface,
            None => surface.insert(
                TerrainSurface::new(loaded.heights.resolution(), loaded.heights.size())
                    .map_err(|error| error.to_string())?,
            ),
        };
        target
            .insert_tile((tile.x, tile.z), loaded)
            .map_err(|error| error.to_string())?;
        tools.get_or_insert(tile_tools);
        tile_paths.insert((tile.x, tile.z), path);
    }
    Ok(LiveTerrain {
        surface: surface.ok_or("A terrain needs at least one tile")?,
        tile_paths,
        tools: tools.unwrap_or_default(),
        data: data.clone(),
    })
}

impl LiveTerrains {
    /// One terrain's live data.
    pub fn get(&self, entity: Entity) -> Option<&LiveTerrain> {
        self.entries.get(&entity)
    }

    /// One terrain's live data, for editing.
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut LiveTerrain> {
        self.entries.get_mut(&entity)
    }

    /// Every loaded terrain.
    pub fn iter(&self) -> impl Iterator<Item = (&Entity, &LiveTerrain)> {
        self.entries.iter()
    }

    /// Forgets everything, as when another scene opens.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Loads `entity`'s terrain now (if it isn't already loaded with the
    /// same tiles) and returns it.
    ///
    /// # Errors
    ///
    /// A message if a tile is missing or can't be read.
    pub fn ensure(
        &mut self,
        entity: Entity,
        data: &TerrainData,
        resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>,
    ) -> Result<&mut LiveTerrain, String> {
        let current = self
            .entries
            .get(&entity)
            .is_some_and(|live| live.data.tiles == data.tiles);
        if !current {
            let live = load(data, resolve)?;
            self.entries.insert(entity, live);
            self.failed.remove(&entity);
            self.stale.insert(entity, Stale::Rebuild);
        }
        self.entries
            .get_mut(&entity)
            .ok_or_else(|| "Terrain could not be loaded".to_owned())
    }

    /// Drops terrains that read any of `paths`, so the next sync reloads
    /// them from disk (after undo or redo rewrote those files).
    pub fn invalidate_paths(&mut self, paths: &[PathBuf]) {
        self.entries
            .retain(|_, live| !live.tile_paths.values().any(|path| paths.contains(path)));
        self.failed.clear();
    }

    /// Queues GPU work after an edit to `entity` that changed `rect`.
    pub fn mark_changed(&mut self, entity: Entity, rect: DirtyRect, paint: bool) {
        let Some(live) = self.entries.get(&entity) else {
            return;
        };
        let update = self.updates.entry(entity).or_default();
        if paint {
            update.splat = engine_renderer::union_dirty(update.splat, Some(rect));
        } else {
            update.chunks.extend(dirty_chunks(&live.surface, rect));
        }
    }

    /// Brings the cache in line with `world`: loads new terrains, reloads
    /// ones whose tiles changed, notes layer changes, forgets removed ones.
    /// Returns one message per terrain that failed to load.
    pub fn refresh(
        &mut self,
        world: &mut World,
        resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>,
    ) -> Vec<String> {
        let terrains: Vec<(Entity, TerrainData, bool)> = world
            .query::<(Entity, &SceneTerrain, Option<&TerrainRenderer>)>()
            .iter(world)
            .map(|(entity, data, renderer)| (entity, data.0.clone(), renderer.is_some()))
            .collect();
        let alive: BTreeSet<Entity> = terrains.iter().map(|(entity, ..)| *entity).collect();
        self.entries.retain(|entity, _| alive.contains(entity));
        self.failed.retain(|entity, _| alive.contains(entity));
        let mut errors = Vec::new();
        for (entity, data, has_renderer) in terrains {
            if self.failed.get(&entity) == Some(&data) {
                continue;
            }
            match self.ensure(entity, &data, resolve) {
                Ok(live) => {
                    let layers_changed = live.data.layers != data.layers
                        || live.data.detail_distance != data.detail_distance;
                    live.data = data;
                    if !has_renderer {
                        self.stale.insert(entity, Stale::Rebuild);
                    } else if layers_changed {
                        self.stale.entry(entity).or_insert(Stale::Layers);
                    }
                }
                Err(error) => {
                    errors.push(error);
                    self.failed.insert(entity, data);
                }
            }
        }
        errors
    }
}

/// The layer images for a terrain, read from the project.
fn layer_images(
    data: &TerrainData,
    resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>,
) -> Vec<engine_renderer::TerrainLayerImage> {
    engine_scene::terrain_layer_images(data, |texture| std::fs::read(resolve(texture)?).ok())
}

/// Refreshes the cache against `world`, then builds, rebuilds or patches
/// each terrain's GPU renderer. Returns load and upload errors.
pub fn sync_gpu(
    gpu: &GpuContext,
    pipeline: &Pipeline,
    world: &mut World,
    terrains: &mut LiveTerrains,
    resolve: &dyn Fn(&AssetRef) -> Option<PathBuf>,
) -> Vec<String> {
    let mut errors = terrains.refresh(world, resolve);
    for (entity, stale) in std::mem::take(&mut terrains.stale) {
        let Some(live) = terrains.entries.get(&entity) else {
            continue;
        };
        let layers = layer_images(&live.data, resolve);
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        match (stale, entity_mut.get_mut::<TerrainRenderer>()) {
            (Stale::Layers, Some(mut renderer)) => {
                renderer.set_layers(gpu, pipeline, &live.surface, &layers);
                renderer.set_detail_distance(&live.surface, live.data.detail_distance);
            }
            _ => match TerrainRenderer::build(
                gpu,
                pipeline,
                &live.surface,
                &layers,
                live.data.detail_distance,
            ) {
                Ok(renderer) => {
                    entity_mut.insert(renderer);
                    // A fresh build already has every change.
                    terrains.updates.remove(&entity);
                }
                Err(error) => errors.push(format!("Could not draw terrain: {error}")),
            },
        }
    }
    for (entity, update) in std::mem::take(&mut terrains.updates) {
        let (Some(live), Some(mut renderer)) = (
            terrains.entries.get(&entity),
            world.get_mut::<TerrainRenderer>(entity),
        ) else {
            continue;
        };
        if !update.chunks.is_empty()
            && let Err(error) = renderer.rewrite_chunks(gpu, &live.surface, &update.chunks)
        {
            errors.push(format!("Could not update terrain: {error}"));
        }
        if let Some(rect) = update.splat {
            renderer.write_splat(gpu, &live.surface, rect);
        }
    }
    errors
}

/// The terrain nearest along `ray` (world space) and the point hit.
/// Disabled and hidden terrains are skipped, and locked ones unless
/// `include_locked`.
pub fn pick_terrain(
    world: &World,
    terrains: &LiveTerrains,
    ray: crate::scene_pick::Ray,
    include_locked: bool,
) -> Option<crate::scene_pick::PickHit> {
    let mut best: Option<(Entity, f32, Vec3)> = None;
    for (entity, live) in terrains.iter() {
        let entity = *entity;
        if world
            .get::<engine_ecs::components::Disabled>(entity)
            .is_some()
            || world
                .get::<engine_ecs::components::Hidden>(entity)
                .is_some()
            || (!include_locked && world.get::<engine_ecs::components::Lock>(entity).is_some())
        {
            continue;
        }
        let model = crate::hierarchy::world_transform(world, entity).to_matrix();
        let inverse = model.inverse();
        if !inverse.is_finite() {
            continue;
        }
        let origin = inverse.transform_point3(ray.origin);
        let direction = inverse.transform_vector3(ray.direction);
        let Some(xz) = live.surface.raycast(origin, direction) else {
            continue;
        };
        let Some(height) = live.surface.height_at(xz.x, xz.y) else {
            continue;
        };
        let point = model.transform_point3(Vec3::new(xz.x, height, xz.y));
        let distance = (point - ray.origin).dot(ray.direction.normalize_or_zero());
        if best.is_none_or(|(_, nearest, _)| distance < nearest) {
            best = Some((entity, distance, point));
        }
    }
    best.map(|(entity, _, point)| crate::scene_pick::PickHit { entity, point })
}
