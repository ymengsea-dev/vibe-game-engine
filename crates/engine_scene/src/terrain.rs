//! Terrain in a scene: the [`TerrainData`] component, the `.terrain` tile
//! files it points at, and loading those into a
//! [`engine_renderer::TerrainSurface`].
//!
//! A terrain object lists its tiles (each a `.terrain` asset at a grid
//! position) and its paint layers. A tile file holds the heights as RON;
//! its paint weights sit beside it in `<stem>.splat` (layers 0 to 3) and
//! `<stem>.splat2` (layers 4 to 7), lossless PNG data under extensions the
//! Asset Browser doesn't treat as images. A missing paint file means the
//! tile is painted entirely with layer 0.
//!
//! Tile files written before paint existed (format 1, no `version` field)
//! load unchanged.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use engine_renderer::{
    BuiltinLayer, Heightmap, MAX_TERRAIN_LAYERS, MAX_TERRAIN_TILES, SplatMap, TerrainSurface,
    TerrainTile,
};
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::format::AssetRef;
use crate::runtime::AssetLibrary;

/// The `.terrain` format version this engine writes.
pub const TERRAIN_FILE_VERSION: u32 = 2;
/// Largest `.terrain` file read.
const MAX_TERRAIN_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
/// Longest layer name, in bytes.
pub const MAX_TERRAIN_LAYER_NAME_BYTES: usize = 64;
/// Smallest and largest detail distance multiplier.
pub const TERRAIN_DETAIL_RANGE: std::ops::RangeInclusive<f32> = 0.25..=8.0;

/// A terrain object: its tiles and paint layers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainData {
    /// The tiles, each a `.terrain` asset at a grid position.
    pub tiles: Vec<TerrainTileData>,
    /// Paint layers, 1 to 8.
    #[serde(default = "default_layers")]
    pub layers: Vec<TerrainLayerData>,
    /// Multiplies the distances where chunks switch to less detail.
    #[serde(default = "default_detail")]
    pub detail_distance: f32,
}

/// One tile of a [`TerrainData`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainTileData {
    /// Grid column.
    pub x: i32,
    /// Grid row.
    pub z: i32,
    /// The `.terrain` file.
    pub asset: AssetRef,
}

/// One paint layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainLayerData {
    /// Display name.
    pub name: String,
    /// A project image to use instead of the built-in texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<AssetRef>,
    /// The built-in texture used when `texture` is unset or can't load.
    #[serde(default)]
    pub builtin: BuiltinLayer,
    /// Linear RGB tint multiplied over the texture.
    #[serde(default = "white")]
    pub tint: [f32; 3],
    /// World units one copy of the texture covers.
    #[serde(default = "default_tiling")]
    pub tiling: f32,
}

impl TerrainLayerData {
    /// A layer using a built-in texture.
    pub fn builtin(layer: BuiltinLayer) -> Self {
        Self {
            name: layer.label().to_owned(),
            texture: None,
            builtin: layer,
            tint: white(),
            tiling: default_tiling(),
        }
    }
}

fn white() -> [f32; 3] {
    [1.0; 3]
}

fn default_tiling() -> f32 {
    4.0
}

fn default_detail() -> f32 {
    1.0
}

/// Grass, Rock, Dirt and Sand: the layers a new terrain starts with.
pub fn default_layers() -> Vec<TerrainLayerData> {
    BuiltinLayer::ALL
        .into_iter()
        .map(TerrainLayerData::builtin)
        .collect()
}

impl TerrainData {
    /// A terrain with one tile at `(0, 0)` and the default layers.
    pub fn single(asset: AssetRef) -> Self {
        Self {
            tiles: vec![TerrainTileData { x: 0, z: 0, asset }],
            layers: default_layers(),
            detail_distance: default_detail(),
        }
    }

    /// The tile at `(x, z)`.
    pub fn tile_at(&self, x: i32, z: i32) -> Option<&TerrainTileData> {
        self.tiles.iter().find(|tile| tile.x == x && tile.z == z)
    }

    /// Every asset this terrain needs: tiles first, then layer images.
    pub fn asset_refs(&self) -> impl Iterator<Item = &AssetRef> {
        self.tiles.iter().map(|tile| &tile.asset).chain(
            self.layers
                .iter()
                .filter_map(|layer| layer.texture.as_ref()),
        )
    }

    /// Checks counts, ids and numbers.
    ///
    /// # Errors
    ///
    /// A description of the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        if self.tiles.is_empty() {
            return Err("a terrain needs at least one tile".to_owned());
        }
        if self.tiles.len() > MAX_TERRAIN_TILES {
            return Err(format!(
                "{} tiles, at most {MAX_TERRAIN_TILES}",
                self.tiles.len()
            ));
        }
        let mut seen = BTreeSet::new();
        for tile in &self.tiles {
            if !seen.insert((tile.x, tile.z)) {
                return Err(format!("two tiles at ({}, {})", tile.x, tile.z));
            }
            tile.asset.parse_id()?;
        }
        if self.layers.is_empty() || self.layers.len() > MAX_TERRAIN_LAYERS {
            return Err(format!(
                "{} paint layers, need 1 to {MAX_TERRAIN_LAYERS}",
                self.layers.len()
            ));
        }
        for layer in &self.layers {
            if layer.name.len() > MAX_TERRAIN_LAYER_NAME_BYTES {
                return Err(format!(
                    "layer name longer than {MAX_TERRAIN_LAYER_NAME_BYTES} bytes"
                ));
            }
            if !layer.tint.iter().all(|c| c.is_finite() && *c >= 0.0) {
                return Err(format!("layer {:?} has a bad tint", layer.name));
            }
            if !(layer.tiling.is_finite() && layer.tiling > 0.0) {
                return Err(format!("layer {:?} tiling must be above zero", layer.name));
            }
            if let Some(texture) = &layer.texture {
                texture.parse_id()?;
            }
        }
        if !(self.detail_distance.is_finite()
            && TERRAIN_DETAIL_RANGE.contains(&self.detail_distance))
        {
            return Err(format!(
                "detail distance must be {} to {}",
                TERRAIN_DETAIL_RANGE.start(),
                TERRAIN_DETAIL_RANGE.end()
            ));
        }
        Ok(())
    }
}

/// Live carrier of an object's authored [`TerrainData`].
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneTerrain(pub TerrainData);

/// Scatter controls saved with a tile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainToolStateData {
    /// Vegetation density.
    #[serde(default = "default_density")]
    pub scatter_density: f32,
    /// Mean vegetation spacing.
    #[serde(default = "default_spacing")]
    pub scatter_spacing: f32,
    /// Deterministic scatter seed.
    #[serde(default = "default_seed")]
    pub scatter_seed: u64,
}

fn default_density() -> f32 {
    1.0
}
fn default_spacing() -> f32 {
    2.0
}
fn default_seed() -> u64 {
    1
}

impl Default for TerrainToolStateData {
    fn default() -> Self {
        Self {
            scatter_density: default_density(),
            scatter_spacing: default_spacing(),
            scatter_seed: default_seed(),
        }
    }
}

/// A `.terrain` tile file: row-major heights plus scatter controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainAsset {
    /// File format; `0` for files written before it was recorded.
    #[serde(default)]
    pub version: u32,
    /// Vertices per side.
    pub resolution: u32,
    /// Patch edge length.
    pub size: f32,
    /// Row-major height samples.
    pub heights: Vec<f32>,
    /// Persistent scatter controls.
    #[serde(default)]
    pub tools: TerrainToolStateData,
}

impl TerrainAsset {
    /// Captures a heightmap with its scatter controls.
    pub fn from_heightmap(heightmap: &Heightmap, tools: TerrainToolStateData) -> Self {
        Self {
            version: TERRAIN_FILE_VERSION,
            resolution: heightmap.resolution(),
            size: heightmap.size(),
            heights: heightmap.heights().to_vec(),
            tools,
        }
    }

    /// The validated heightmap.
    ///
    /// # Errors
    ///
    /// [`engine_renderer::RendererError::InvalidHeightmap`] for bad
    /// dimensions or samples.
    pub fn heightmap(&self) -> Result<Heightmap, engine_renderer::RendererError> {
        Heightmap::from_heights(self.resolution, self.size, self.heights.clone())
    }

    /// Parses and validates tile file text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Deserialize`] for oversized, malformed or invalid data.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SceneError> {
        if bytes.len() as u64 > MAX_TERRAIN_SOURCE_BYTES {
            return Err(SceneError::Deserialize(
                "terrain source is too large".to_owned(),
            ));
        }
        let text =
            std::str::from_utf8(bytes).map_err(|err| SceneError::Deserialize(err.to_string()))?;
        let asset: Self =
            ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))?;
        if asset.version > TERRAIN_FILE_VERSION {
            return Err(SceneError::Deserialize(format!(
                "terrain file version {} is newer than this engine supports",
                asset.version
            )));
        }
        asset
            .heightmap()
            .map_err(|err| SceneError::Deserialize(err.to_string()))?;
        Ok(asset)
    }

    /// Loads and validates a tile file; oversized data is rejected before
    /// it's read.
    ///
    /// # Errors
    ///
    /// [`SceneError::Io`] or [`SceneError::Deserialize`].
    pub fn load(path: &Path) -> Result<Self, SceneError> {
        let io_error = |err: io::Error| SceneError::Io(format!("{}: {err}", path.display()));
        let file = std::fs::File::open(path).map_err(io_error)?;
        if file.metadata().map_err(io_error)?.len() > MAX_TERRAIN_SOURCE_BYTES {
            return Err(SceneError::Deserialize(
                "terrain source is too large".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_TERRAIN_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        Self::from_bytes(&bytes)
    }

    /// Writes this tile file atomically as RON.
    ///
    /// # Errors
    ///
    /// The underlying I/O error.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(io::Error::other)?;
        crate::write_atomic(path, text.as_bytes())
    }
}

/// The two paint files beside a tile file.
pub fn splat_paths(terrain: &Path) -> [PathBuf; 2] {
    [
        terrain.with_extension("splat"),
        terrain.with_extension("splat2"),
    ]
}

/// The two paint files beside a `/`-separated store-relative tile path.
pub fn splat_relative_paths(terrain: &str) -> [String; 2] {
    let stem = terrain.strip_suffix(".terrain").unwrap_or(terrain);
    [format!("{stem}.splat"), format!("{stem}.splat2")]
}

/// Builds a tile from its file bytes and optional paint bytes. Paint that
/// can't be decoded or has the wrong size is ignored with a warning, so a
/// damaged paint file never hides the terrain.
///
/// # Errors
///
/// [`SceneError::Deserialize`] if the heights are invalid.
pub fn tile_from_bytes(
    terrain: &[u8],
    splat: Option<&[u8]>,
    splat2: Option<&[u8]>,
) -> Result<(TerrainTile, TerrainToolStateData), SceneError> {
    let asset = TerrainAsset::from_bytes(terrain)?;
    let heights = asset
        .heightmap()
        .map_err(|err| SceneError::Deserialize(err.to_string()))?;
    let resolution = asset.resolution;
    let decode = |bytes: Option<&[u8]>| -> Option<Vec<u8>> {
        let image = engine_renderer::decode_rgba8(bytes?)
            .map_err(|err| tracing::warn!("terrain paint can't be read: {err}"))
            .ok()?;
        if image.width() != resolution || image.height() != resolution {
            tracing::warn!("terrain paint size doesn't match its heights; ignoring it");
            return None;
        }
        Some(image.into_raw())
    };
    let splat = match (decode(splat), decode(splat2)) {
        (Some(first), second) => SplatMap::from_rgba(resolution, &first, second.as_deref())
            .unwrap_or_else(|_| SplatMap::new(resolution)),
        (None, Some(second)) => {
            SplatMap::from_rgba(resolution, &vec![0; second.len()], Some(&second))
                .unwrap_or_else(|_| SplatMap::new(resolution))
        }
        (None, None) => SplatMap::new(resolution),
    };
    Ok((TerrainTile { heights, splat }, asset.tools))
}

/// Loads a tile file and its paint files.
///
/// # Errors
///
/// [`SceneError::Io`] or [`SceneError::Deserialize`] for the tile file.
/// Missing paint files are fine.
pub fn load_tile_file(path: &Path) -> Result<(TerrainTile, TerrainToolStateData), SceneError> {
    let asset = TerrainAsset::load(path)?;
    let text = ron::to_string(&asset).map_err(|err| SceneError::Serialize(err.to_string()))?;
    let [first, second] = splat_paths(path);
    let read = |path: &Path| std::fs::read(path).ok();
    tile_from_bytes(
        text.as_bytes(),
        read(&first).as_deref(),
        read(&second).as_deref(),
    )
}

/// Writes a tile's heights and paint. The second paint file is written
/// only when layers 4 to 7 are used or it already exists.
///
/// # Errors
///
/// The first I/O or encoding error.
pub fn save_tile_file(
    path: &Path,
    tile: &TerrainTile,
    tools: &TerrainToolStateData,
) -> io::Result<()> {
    TerrainAsset::from_heightmap(&tile.heights, tools.clone()).save(path)?;
    let resolution = tile.splat.resolution();
    let [first, second] = splat_paths(path);
    let encode = |map: usize| {
        engine_renderer::encode_rgba8_png(resolution, resolution, &tile.splat.rgba(map))
            .map_err(io::Error::other)
    };
    crate::write_atomic(&first, &encode(0)?)?;
    if tile.splat.uses_second_map() || second.exists() {
        crate::write_atomic(&second, &encode(1)?)?;
    }
    Ok(())
}

/// The layer images for `data`: each layer's project image when `read`
/// returns one that decodes, its built-in texture otherwise.
pub fn terrain_layer_images(
    data: &TerrainData,
    read: impl Fn(&AssetRef) -> Option<Vec<u8>>,
) -> Vec<engine_renderer::TerrainLayerImage> {
    data.layers
        .iter()
        .take(MAX_TERRAIN_LAYERS)
        .map(|layer| {
            layer
                .texture
                .as_ref()
                .and_then(|texture| {
                    let bytes = read(texture)?;
                    engine_renderer::TerrainLayerImage::from_image_bytes(&bytes, layer.tint, layer.tiling)
                        .map_err(|err| {
                            tracing::warn!(layer = %layer.name, "terrain layer image can't be read: {err}");
                        })
                        .ok()
                })
                .unwrap_or_else(|| {
                    engine_renderer::TerrainLayerImage::builtin(layer.builtin, layer.tint, layer.tiling)
                })
        })
        .collect()
}

/// Loads every tile of `data` from `library` into one surface.
///
/// # Errors
///
/// [`SceneError::Resolve`] if a tile is missing or the tiles don't fit
/// together; [`SceneError::Deserialize`] for a bad tile file.
pub fn load_surface(
    library: &AssetLibrary,
    data: &TerrainData,
) -> Result<TerrainSurface, SceneError> {
    let mut surface: Option<TerrainSurface> = None;
    let mut tiles: Vec<&TerrainTileData> = data.tiles.iter().collect();
    tiles.sort_by_key(|tile| (tile.z, tile.x));
    for tile in tiles {
        let bytes = library.read(&tile.asset.id)?.ok_or_else(|| {
            SceneError::Resolve(format!(
                "terrain tile {} is not in the project",
                tile.asset.id
            ))
        })?;
        let [first, second] = library
            .relative_path(&tile.asset.id)
            .map(splat_relative_paths)
            .map_or([None, None], |paths| {
                paths.map(|path| library.read_path(&path).ok().flatten())
            });
        let (loaded, _) = tile_from_bytes(&bytes, first.as_deref(), second.as_deref())?;
        let target = match &mut surface {
            Some(surface) => surface,
            None => surface.insert(
                TerrainSurface::new(loaded.heights.resolution(), loaded.heights.size())
                    .map_err(|err| SceneError::Resolve(err.to_string()))?,
            ),
        };
        target
            .insert_tile((tile.x, tile.z), loaded)
            .map_err(|err| SceneError::Resolve(err.to_string()))?;
    }
    surface.ok_or_else(|| SceneError::Resolve("a terrain needs at least one tile".to_owned()))
}

/// Converts terrains made before terrain had its own component: an object
/// showing `terrain/<Name>.gltf`, with `<Name>.terrain` beside it. Each
/// becomes a one-tile [`TerrainData`] using that `.terrain` file as tile
/// `(0, 0)` (which covers the same ground the model did), loses its model
/// reference, and a triangle-mesh collider on that model becomes a
/// [`crate::ColliderShape::Terrain`] keeping its settings. Nothing else
/// about the object changes, and no file is touched.
///
/// `tile_id` maps the `.terrain` path (relative to the assets folder, `/`
/// separated) to its asset id, or `None` if it has none; such objects are
/// left alone. Returns how many objects were converted.
pub fn migrate_legacy_terrains(
    scene: &mut crate::Scene,
    tile_id: &dyn Fn(&str) -> Option<String>,
) -> usize {
    let mut converted = 0;
    for entity in &mut scene.entities {
        if entity.terrain.is_some() {
            continue;
        }
        let Some(source) = entity.asset_source.as_deref() else {
            continue;
        };
        let path = Path::new(source);
        let is_terrain_model = path
            .extension()
            .is_some_and(|extension| extension == "gltf")
            && path
                .parent()
                .is_some_and(|parent| parent.ends_with("terrain"));
        if !is_terrain_model {
            continue;
        }
        let tile = path
            .with_extension("terrain")
            .to_string_lossy()
            .replace('\\', "/");
        let Some(id) = tile_id(&tile) else {
            continue;
        };
        let model_ids: Vec<String> = entity
            .asset_id
            .iter()
            .cloned()
            .chain(
                entity
                    .mesh_renderer
                    .iter()
                    .map(|renderer| renderer.mesh.id.clone()),
            )
            .collect();
        if let Some(collider) = &mut entity.collider
            && let crate::ColliderShape::TriMesh { mesh } = &collider.shape
            && model_ids.contains(&mesh.id)
        {
            collider.shape = crate::ColliderShape::Terrain;
        }
        entity.terrain = Some(TerrainData::single(AssetRef { id }));
        entity.asset_source = None;
        entity.asset_id = None;
        entity.mesh_renderer = None;
        converted += 1;
    }
    converted
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_renderer::Brush;
    use glam::Vec2;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vge-scene-terrain-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";

    #[test]
    fn a_format_1_tile_file_still_loads() {
        // Written by Studio before paint layers existed.
        let text = "(resolution: 2, size: 4.0, heights: [0.0, 1.0, 2.0, 3.0], tools: (scatter_density: 0.5))";
        let asset = TerrainAsset::from_bytes(text.as_bytes()).unwrap();
        assert_eq!(asset.version, 0);
        assert_eq!(asset.tools.scatter_density, 0.5);
        assert_eq!(asset.tools.scatter_seed, 1);
        let (tile, _) = tile_from_bytes(text.as_bytes(), None, None).unwrap();
        assert_eq!(tile.heights.heights(), &[0.0, 1.0, 2.0, 3.0]);
        assert!(tile.splat.weights().iter().all(|t| t[0] == 255));
        assert!(TerrainAsset::from_bytes(b"(resolution: 1, size: 4.0, heights: [0.0])").is_err());
        assert!(
            TerrainAsset::from_bytes(
                b"(version: 99, resolution: 2, size: 1.0, heights: [0.0, 0.0, 0.0, 0.0])"
            )
            .is_err()
        );
    }

    #[test]
    fn paint_files_round_trip_and_a_missing_one_means_layer_0() {
        let dir = temp_dir("paint");
        let path = dir.join("tile_0_0.terrain");
        let mut surface = TerrainSurface::new(9, 8.0).unwrap();
        surface
            .insert_tile((0, 0), TerrainTile::flat(9, 8.0).unwrap())
            .unwrap();
        surface.paint(&Brush::new(Vec2::ZERO, 3.0, 0.8), 2);
        surface.raise_lower(&Brush::new(Vec2::ZERO, 3.0, 1.0));
        let tile = surface.tile((0, 0)).unwrap().clone();
        save_tile_file(&path, &tile, &TerrainToolStateData::default()).unwrap();
        let [first, second] = splat_paths(&path);
        assert!(first.exists());
        assert!(!second.exists(), "layers 4 to 7 are unused");
        let (back, _) = load_tile_file(&path).unwrap();
        assert_eq!(back, tile);

        surface.paint(&Brush::new(Vec2::ZERO, 2.0, 0.5), 6);
        let tile = surface.tile((0, 0)).unwrap().clone();
        save_tile_file(&path, &tile, &TerrainToolStateData::default()).unwrap();
        assert!(second.exists());
        assert_eq!(load_tile_file(&path).unwrap().0, tile);

        std::fs::remove_file(&first).unwrap();
        std::fs::remove_file(&second).unwrap();
        let (bare, _) = load_tile_file(&path).unwrap();
        assert!(bare.splat.weights().iter().all(|t| t[0] == 255));
        // Damaged paint is ignored rather than failing the tile.
        std::fs::write(&first, b"not a png").unwrap();
        assert!(load_tile_file(&path).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn splat_relative_paths_sit_beside_the_tile() {
        assert_eq!(
            splat_relative_paths("terrain/Hills/tile_1_-2.terrain"),
            [
                "terrain/Hills/tile_1_-2.splat",
                "terrain/Hills/tile_1_-2.splat2"
            ]
        );
    }

    #[test]
    fn terrain_data_round_trips_and_validation_catches_bad_values() {
        let data = TerrainData::single(AssetRef { id: ID.to_owned() });
        data.validate().unwrap();
        let text = ron::to_string(&data).unwrap();
        assert_eq!(ron::from_str::<TerrainData>(&text).unwrap(), data);
        // Old-style minimal text gets default layers and detail.
        let minimal: TerrainData =
            ron::from_str(&format!("(tiles: [(x: 0, z: 0, asset: (id: \"{ID}\"))])")).unwrap();
        assert_eq!(minimal.layers.len(), 4);
        assert_eq!(minimal.detail_distance, 1.0);

        let mut twice = data.clone();
        twice.tiles.push(twice.tiles[0].clone());
        assert!(twice.validate().unwrap_err().contains("two tiles"));
        let mut none = data.clone();
        none.layers.clear();
        assert!(none.validate().is_err());
        let mut nine = data.clone();
        nine.layers = (0..9)
            .map(|_| TerrainLayerData::builtin(BuiltinLayer::Rock))
            .collect();
        assert!(nine.validate().is_err());
        let mut nan = data.clone();
        nan.layers[0].tiling = f32::NAN;
        assert!(nan.validate().is_err());
        let mut far = data.clone();
        far.detail_distance = 100.0;
        assert!(far.validate().is_err());
        let mut empty = data;
        empty.tiles.clear();
        assert!(empty.validate().is_err());
    }

    fn legacy_scene() -> crate::Scene {
        let model = "9a9a9a9a-0000-4000-8000-000000000001";
        let text = format!(
            r#"(version: 5, entities: [
                (name: Some("Ground"), object_id: Some("5b5b5b5b-0000-4000-8000-000000000002"), tags: ["floor"],
                 transform: Some((translation: (1.0, 2.0, 3.0), rotation: (0.0, 0.0, 0.0, 1.0), scale: (2.0, 1.0, 2.0))),
                 asset_source: Some("terrain/Terrain.gltf"), asset_id: Some("{model}"),
                 mesh_renderer: Some((mesh: (id: "{model}"), material: (id: "{model}"))),
                 collider: Some((shape: TriMesh(mesh: (id: "{model}")), friction: 0.8))),
                (name: Some("Rock"), asset_source: Some("props/rock.gltf"), asset_id: Some("{model}")),
                (name: Some("Lost"), asset_source: Some("terrain/Missing.gltf")),
            ])"#
        );
        crate::Scene::from_ron_str(&text).unwrap()
    }

    #[test]
    fn legacy_terrains_convert_keeping_everything_else() {
        let mut scene = legacy_scene();
        let original = scene.clone();
        let lookup = |path: &str| (path == "terrain/Terrain.terrain").then(|| ID.to_owned());
        assert_eq!(migrate_legacy_terrains(&mut scene, &lookup), 1);
        let ground = &scene.entities[0];
        assert_eq!(
            ground.terrain,
            Some(TerrainData::single(AssetRef { id: ID.to_owned() }))
        );
        assert!(ground.asset_source.is_none() && ground.asset_id.is_none());
        assert!(ground.mesh_renderer.is_none());
        let collider = ground.collider.as_ref().unwrap();
        assert_eq!(collider.shape, crate::ColliderShape::Terrain);
        assert_eq!(collider.friction, 0.8);
        assert_eq!(ground.transform, original.entities[0].transform);
        assert_eq!(ground.object_id, original.entities[0].object_id);
        assert_eq!(ground.tags, original.entities[0].tags);
        assert_eq!(ground.name, original.entities[0].name);
        // A model outside a terrain folder, and one with no tile file.
        assert_eq!(scene.entities[1], original.entities[1]);
        assert_eq!(scene.entities[2], original.entities[2]);
        scene.validate().unwrap();
        // Running it again changes nothing.
        let once = scene.clone();
        assert_eq!(migrate_legacy_terrains(&mut scene, &lookup), 0);
        assert_eq!(scene, once);
    }

    #[test]
    fn terrain_survives_instantiate_and_capture() {
        let data = TerrainData::single(AssetRef { id: ID.to_owned() });
        let scene = crate::Scene {
            version: crate::CURRENT_SCENE_VERSION,
            entities: vec![crate::SceneEntity {
                name: Some("Ground".into()),
                terrain: Some(data.clone()),
                ..crate::SceneEntity::default()
            }],
            nav_grid: None,
            lighting: None,
            environment: Default::default(),
        };
        scene.validate().unwrap();
        let mut world = bevy_ecs::world::World::new();
        scene.instantiate(&mut world);
        assert_eq!(
            crate::Scene::from_world(&mut world).entities[0].terrain,
            Some(data)
        );
    }

    #[test]
    fn load_surface_reads_tiles_and_paint_from_a_library() {
        let dir = temp_dir("library");
        let assets = dir.join("assets");
        let tiles = assets.join("terrain/Hills");
        std::fs::create_dir_all(&tiles).unwrap();
        let ids = [
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        ];
        let mut surface = TerrainSurface::new(5, 4.0).unwrap();
        for x in 0..2 {
            surface
                .insert_tile((x, 0), TerrainTile::flat(5, 4.0).unwrap())
                .unwrap();
        }
        surface.raise_lower(&Brush::new(Vec2::new(2.0, 0.0), 1.5, 1.0));
        surface.paint(&Brush::new(Vec2::new(2.0, 0.0), 1.5, 1.0), 1);
        for (x, id) in ids.iter().enumerate() {
            let path = tiles.join(format!("tile_{x}_0.terrain"));
            save_tile_file(
                &path,
                surface.tile((x as i32, 0)).unwrap(),
                &TerrainToolStateData::default(),
            )
            .unwrap();
            let meta = dir.join(format!(
                ".studio/assets/terrain/Hills/tile_{x}_0.terrain.meta"
            ));
            std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
            std::fs::write(&meta, format!("(id: \"{id}\", source_hash: 0)")).unwrap();
        }
        let library = AssetLibrary::index(&assets).unwrap();
        let data = TerrainData {
            tiles: ids
                .iter()
                .enumerate()
                .map(|(x, id)| TerrainTileData {
                    x: x as i32,
                    z: 0,
                    asset: AssetRef {
                        id: (*id).to_owned(),
                    },
                })
                .collect(),
            ..TerrainData::single(AssetRef {
                id: ids[0].to_owned(),
            })
        };
        let loaded = load_surface(&library, &data).unwrap();
        assert_eq!(loaded, surface);
        let missing = TerrainData::single(AssetRef { id: ID.to_owned() });
        assert!(load_surface(&library, &missing).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
