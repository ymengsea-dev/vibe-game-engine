//! Bounded terrain and vegetation authoring state.
//!
//! This module deliberately keeps authoring data separate from GPU resources:
//! a caller owns the [`Heightmap`] and applies one bounded operation per user
//! gesture. Each stroke returns a [`TerrainEdit`] holding the terrain before
//! and after, which the editor's undo history replays with [`write_terrain`].

use engine_renderer::{Brush, BrushFalloff, Heightmap, ScatterArea, ScatterConfig, scatter};
use glam::{Mat4, Vec2, Vec3, Vec4};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// One viewport gesture; it keeps the source heightmap in memory while
/// pointer samples accumulate, then the host writes one mesh update.
pub struct TerrainStroke {
    /// Entity selected when the stroke began.
    pub entity: engine_ecs::prelude::Entity,
    /// Model path relative to the project's assets root.
    pub relative_model: PathBuf,
    /// Source surface used to map pointer rays into local coordinates.
    pub heightmap: Heightmap,
    /// Bounded brush samples collected during the gesture.
    pub centers: Vec<Vec2>,
}

impl TerrainStroke {
    /// Samples a pointer segment densely enough to avoid gaps in a fast drag.
    pub fn record(&mut self, center: Vec2, spacing: f32) {
        if !center.is_finite()
            || !spacing.is_finite()
            || spacing <= 0.0
            || self.centers.len() >= 1024
        {
            return;
        }
        let Some(previous) = self.centers.last().copied() else {
            self.centers.push(center);
            return;
        };
        let distance = previous.distance(center);
        if distance < spacing {
            return;
        }
        let steps = ((distance / spacing).ceil() as usize).min(1024 - self.centers.len());
        for step in 1..=steps {
            self.centers
                .push(previous.lerp(center, step as f32 / steps as f32));
        }
    }
}

/// The operation performed by a terrain brush.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerrainBrushMode {
    /// Add the brush strength to the terrain.
    #[default]
    Raise,
    /// Subtract the brush strength from the terrain.
    Lower,
    /// Move terrain toward the neighbourhood average.
    Smooth,
    /// Move terrain toward [`TerrainToolState::flatten_target`].
    Flatten,
}

/// Persistent editor controls for terrain sculpting and vegetation previews.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainToolState {
    /// Active sculpt operation.
    pub brush_mode: TerrainBrushMode,
    /// Brush radius in terrain local units.
    pub radius: f32,
    /// Brush strength; bounded before applying.
    pub strength: f32,
    /// Height used by flatten mode.
    pub flatten_target: f32,
    /// Edge falloff.
    pub falloff: BrushFalloff,
    /// Scatter seed, stable across previews.
    pub scatter_seed: u64,
    /// Scatter density in `[0, 1]`.
    pub scatter_density: f32,
    /// Scatter spacing in world units.
    pub scatter_spacing: f32,
}

/// Serializable terrain authoring asset. Heights are row-major and bounded
/// by the renderer's validated [`Heightmap`] constructor on load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainAsset {
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

/// Persistent vegetation controls stored alongside a terrain asset.
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

impl TerrainAsset {
    /// Captures a heightmap and the persistent vegetation controls.
    pub fn from_heightmap(heightmap: &Heightmap, state: &TerrainToolState) -> Self {
        Self {
            resolution: heightmap.resolution(),
            size: heightmap.size(),
            heights: heightmap.heights().to_vec(),
            tools: TerrainToolStateData {
                scatter_density: state.scatter_density,
                scatter_spacing: state.scatter_spacing,
                scatter_seed: state.scatter_seed,
            },
        }
    }

    /// Rebuilds a validated heightmap and sanitized controls.
    pub fn into_parts(
        self,
    ) -> Result<(Heightmap, TerrainToolState), engine_renderer::RendererError> {
        let heightmap = Heightmap::from_heights(self.resolution, self.size, self.heights)?;
        let mut state = TerrainToolState {
            scatter_density: self.tools.scatter_density,
            scatter_spacing: self.tools.scatter_spacing,
            scatter_seed: self.tools.scatter_seed,
            ..TerrainToolState::default()
        };
        state.sanitize();
        Ok((heightmap, state))
    }

    /// Writes this asset atomically as RON, creating its parent directory.
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?;
        engine_scene::write_atomic(path, text.as_bytes())
    }

    /// Loads and validates an asset; malformed or oversized data is rejected.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > MAX_SOURCE_BYTES {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "terrain source is too large").into(),
            );
        }
        let mut text = String::new();
        file.take(MAX_SOURCE_BYTES + 1).read_to_string(&mut text)?;
        if text.len() as u64 > MAX_SOURCE_BYTES {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "terrain source is too large").into(),
            );
        }
        let asset: Self = ron::from_str(&text)?;
        asset.clone().into_parts()?;
        Ok(asset)
    }
}

/// Creates a flat, editable terrain and the self-contained mesh that Play
/// and export already know how to load. Never replaces an existing pair.
pub fn write_new_terrain(assets_dir: &Path, state: &TerrainToolState) -> io::Result<PathBuf> {
    let directory = assets_dir.join("terrain");
    std::fs::create_dir_all(&directory)?;
    let heightmap = Heightmap::new(65, 32.0).map_err(io::Error::other)?;
    let authored = TerrainAsset::from_heightmap(&heightmap, state);
    let authored_bytes = ron::ser::to_string_pretty(&authored, ron::ser::PrettyConfig::default())
        .map_err(io::Error::other)?;
    let model_bytes = terrain_mesh_bytes(&heightmap)?;

    for index in 0..10_000 {
        let stem = if index == 0 {
            "Terrain".to_owned()
        } else {
            format!("Terrain {index}")
        };
        let model_path = directory.join(format!("{stem}.gltf"));
        let authored_path = directory.join(format!("{stem}.terrain"));
        if model_path.exists() || authored_path.exists() {
            continue;
        }
        let mut authoring_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&authored_path)?;
        if let Err(error) = authoring_file.write_all(authored_bytes.as_bytes()) {
            drop(authoring_file);
            let _ = std::fs::remove_file(&authored_path);
            return Err(error);
        }
        drop(authoring_file);
        let model_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&model_path);
        let mut model_file = match model_file {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_file(&authored_path);
                return Err(error);
            }
        };
        if let Err(error) = model_file.write_all(&model_bytes) {
            drop(model_file);
            let _ = std::fs::remove_file(&model_path);
            let _ = std::fs::remove_file(&authored_path);
            return Err(error);
        }
        return Ok(model_path);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many terrains already exist",
    ))
}

fn terrain_mesh_bytes(heightmap: &Heightmap) -> io::Result<Vec<u8>> {
    let (vertices, indices) = heightmap.mesh_data();
    engine_asset::export_gltf(
        &[engine_asset::ExportMesh {
            name: "Terrain",
            vertices: &vertices,
            indices: &indices,
            material: Some(0),
        }],
        &[engine_asset::ExportMaterial::new("Terrain")],
    )
    .map_err(io::Error::other)
}

/// One sculpt stroke: the terrain source before and after, for undo/redo.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainEdit {
    /// Absolute path of the terrain's generated `.gltf` model; the source
    /// `.terrain` file sits beside it.
    pub model_path: PathBuf,
    /// The terrain before the stroke.
    pub before: TerrainAsset,
    /// The terrain after the stroke.
    pub after: TerrainAsset,
}

impl TerrainEdit {
    /// Approximate memory held by this edit, for bounding the undo history.
    pub fn byte_size(&self) -> usize {
        (self.before.heights.len() + self.after.heights.len()) * std::mem::size_of::<f32>()
    }
}

fn is_terrain_model(model_path: &Path) -> bool {
    model_path
        .extension()
        .is_some_and(|extension| extension == "gltf")
}

/// Writes `asset` as the terrain's `.terrain` source and regenerates its mesh.
/// Used to restore a terrain during undo/redo.
pub fn write_terrain(model_path: &Path, asset: &TerrainAsset) -> io::Result<()> {
    if !is_terrain_model(model_path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid terrain model",
        ));
    }
    let (heightmap, _) = asset.clone().into_parts().map_err(io::Error::other)?;
    let model_bytes = terrain_mesh_bytes(&heightmap)?;
    asset.save(&model_path.with_extension("terrain"))?;
    // shortcut: these two files cannot be renamed atomically together;
    // regenerate the model from the .terrain source after an interrupted write.
    engine_scene::write_atomic(model_path, &model_bytes)
}

/// Applies one bounded viewport stroke and regenerates the Play/export mesh.
/// Returns `None` for a stroke that did not touch the terrain.
pub fn sculpt_terrain(
    model_path: &Path,
    state: &TerrainToolState,
    centers: &[Vec2],
) -> io::Result<Option<TerrainEdit>> {
    if !is_terrain_model(model_path)
        || centers.len() > 1024
        || centers.iter().any(|center| !center.is_finite())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid terrain model or stroke",
        ));
    }
    if centers.is_empty() {
        return Ok(None);
    }
    let before =
        TerrainAsset::load(&model_path.with_extension("terrain")).map_err(io::Error::other)?;
    let (mut heightmap, _) = before.clone().into_parts().map_err(io::Error::other)?;
    let mut changed = false;
    for &center in centers {
        changed |= state.apply_brush(&mut heightmap, center);
    }
    if !changed {
        return Ok(None);
    }
    let after = TerrainAsset::from_heightmap(&heightmap, state);
    write_terrain(model_path, &after)?;
    Ok(Some(TerrainEdit {
        model_path: model_path.to_path_buf(),
        before,
        after,
    }))
}

/// First visible surface hit of a camera ray in terrain-local coordinates.
/// Bounded sampling follows the heightmap grid, then refines the crossing.
pub fn raycast_heightmap(heightmap: &Heightmap, origin: Vec3, direction: Vec3) -> Option<Vec2> {
    if !origin.is_finite() || !direction.is_finite() || direction.length_squared() < 0.000001 {
        return None;
    }
    let direction = direction.normalize();
    let half = heightmap.size() * 0.5;
    let mut near = 0.0f32;
    let mut far = 10_000.0f32;
    for (position, velocity) in [(origin.x, direction.x), (origin.z, direction.z)] {
        if velocity.abs() < 0.000001 {
            if position.abs() > half {
                return None;
            }
        } else {
            let a = (-half - position) / velocity;
            let b = (half - position) / velocity;
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
    }
    if far < near {
        return None;
    }
    let difference = |distance: f32| {
        let point = origin + direction * distance;
        point.y - heightmap.height_at(point.x, point.z)
    };
    let mut previous_t = near;
    let mut previous = difference(near);
    if previous.abs() < 0.000001 {
        let hit = origin + direction * near;
        return Some(Vec2::new(hit.x, hit.z));
    }
    let steps = (heightmap.resolution() * 2).clamp(64, 4096);
    for index in 1..=steps {
        let current_t = near + (far - near) * index as f32 / steps as f32;
        let current = difference(current_t);
        if previous > 0.0 && current <= 0.0 {
            let mut low = previous_t;
            let mut high = current_t;
            for _ in 0..16 {
                let middle = (low + high) * 0.5;
                if difference(middle) > 0.0 {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            let hit = origin + direction * ((low + high) * 0.5);
            return Some(Vec2::new(hit.x, hit.z));
        }
        previous_t = current_t;
        previous = current;
    }
    None
}

/// Maps a Scene View pointer position to the first hit on a transformed
/// terrain. `pointer` and `size` are in viewport-local logical pixels.
pub fn terrain_hit_from_screen(
    view_projection: Mat4,
    model: Mat4,
    heightmap: &Heightmap,
    pointer: Vec2,
    size: Vec2,
) -> Option<Vec2> {
    if !view_projection.is_finite()
        || !model.is_finite()
        || !pointer.is_finite()
        || !size.is_finite()
        || size.min_element() <= 0.0
        || pointer.min_element() < 0.0
        || pointer.x > size.x
        || pointer.y > size.y
    {
        return None;
    }
    let inverse_view = view_projection.inverse();
    let inverse_model = model.inverse();
    if !inverse_view.is_finite() || !inverse_model.is_finite() {
        return None;
    }
    let ndc = Vec2::new(
        pointer.x / size.x * 2.0 - 1.0,
        1.0 - pointer.y / size.y * 2.0,
    );
    let unproject = |depth| {
        let clip = inverse_view * Vec4::new(ndc.x, ndc.y, depth, 1.0);
        (clip.w.abs() > f32::EPSILON).then(|| clip.truncate() / clip.w)
    };
    let near = unproject(0.0)?;
    let far = unproject(1.0)?;
    let origin = inverse_model.transform_point3(near);
    let direction = inverse_model.transform_vector3(far - near);
    raycast_heightmap(heightmap, origin, direction)
}

impl Default for TerrainToolStateData {
    fn default() -> Self {
        Self {
            scatter_density: 1.0,
            scatter_spacing: 2.0,
            scatter_seed: 1,
        }
    }
}

impl Default for TerrainToolState {
    fn default() -> Self {
        Self {
            brush_mode: TerrainBrushMode::Raise,
            radius: 2.0,
            strength: 0.25,
            flatten_target: 0.0,
            falloff: BrushFalloff::Smooth,
            scatter_seed: 1,
            scatter_density: 1.0,
            scatter_spacing: 2.0,
        }
    }
}

impl TerrainToolState {
    /// Clamps interactive controls to safe, finite ranges.
    pub fn sanitize(&mut self) {
        if !self.radius.is_finite() {
            self.radius = 2.0;
        }
        if !self.strength.is_finite() {
            self.strength = 0.25;
        }
        if !self.flatten_target.is_finite() {
            self.flatten_target = 0.0;
        }
        if !self.scatter_density.is_finite() {
            self.scatter_density = 1.0;
        }
        if !self.scatter_spacing.is_finite() {
            self.scatter_spacing = 2.0;
        }
        self.radius = self.radius.clamp(0.01, 10_000.0);
        self.strength = self.strength.clamp(-10_000.0, 10_000.0);
        self.scatter_density = self.scatter_density.clamp(0.0, 1.0);
        self.scatter_spacing = self.scatter_spacing.clamp(0.01, 10_000.0);
    }

    /// Applies one sculpt gesture at `center`. Returns whether the heightmap changed.
    pub fn apply_brush(&self, heightmap: &mut Heightmap, center: Vec2) -> bool {
        let mut state = self.clone();
        state.sanitize();
        let brush = Brush {
            center,
            radius: state.radius,
            strength: state.strength,
            falloff: state.falloff,
        };
        let before = heightmap.heights().to_vec();
        match state.brush_mode {
            TerrainBrushMode::Raise => heightmap.raise_lower(&brush),
            TerrainBrushMode::Lower => heightmap.raise_lower(&Brush {
                strength: -brush.strength.abs(),
                ..brush
            }),
            TerrainBrushMode::Smooth => heightmap.smooth(&brush),
            TerrainBrushMode::Flatten => heightmap.flatten(&brush, state.flatten_target),
        }
        before != heightmap.heights()
    }

    /// Builds a deterministic, bounded vegetation preview over `area`.
    pub fn scatter_preview(
        &self,
        area: ScatterArea,
        terrain: Option<&Heightmap>,
    ) -> Result<Vec<engine_utils::Transform>, engine_renderer::ScatterError> {
        let mut state = self.clone();
        state.sanitize();
        let mut config = ScatterConfig::new(area, state.scatter_spacing);
        config.density = state.scatter_density;
        config.yaw_random = true;
        scatter(&config, state.scatter_seed, terrain)
    }
}

/// Compact UI for the authoring controls. Applying a brush remains an
/// explicit caller action so a cancelled gesture cannot dirty the scene.
pub fn show_controls(ui: &mut egui::Ui, state: &mut TerrainToolState) {
    state.sanitize();
    egui::ComboBox::from_label("Brush")
        .selected_text(format!("{:?}", state.brush_mode))
        .show_ui(ui, |ui| {
            for mode in [
                TerrainBrushMode::Raise,
                TerrainBrushMode::Lower,
                TerrainBrushMode::Smooth,
                TerrainBrushMode::Flatten,
            ] {
                ui.selectable_value(&mut state.brush_mode, mode, format!("{mode:?}"));
            }
        });
    ui.add(egui::Slider::new(&mut state.radius, 0.01..=100.0).text("Radius"));
    ui.add(egui::Slider::new(&mut state.strength, -10.0..=10.0).text("Strength"));
    if state.brush_mode == TerrainBrushMode::Flatten {
        ui.add(egui::DragValue::new(&mut state.flatten_target).prefix("Target "));
    }
    ui.separator();
    ui.label("Foliage scatter");
    ui.add(egui::Slider::new(&mut state.scatter_density, 0.0..=1.0).text("Density"));
    ui.add(
        egui::DragValue::new(&mut state.scatter_spacing)
            .prefix("Spacing ")
            .range(0.01..=10000.0),
    );
    ui.add(egui::DragValue::new(&mut state.scatter_seed).prefix("Seed "));
}

/// Returns a surface-aligned preview position for a vegetation transform.
pub fn preview_position(heightmap: &Heightmap, position: Vec2) -> Vec3 {
    Vec3::new(
        position.x,
        heightmap.height_at(position.x, position.y),
        position.y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn lower_is_the_inverse_of_raise() {
        let mut raised = Heightmap::new(9, 8.0).unwrap();
        let mut lowered = raised.clone();
        let mut state = TerrainToolState {
            brush_mode: TerrainBrushMode::Raise,
            ..TerrainToolState::default()
        };
        assert!(state.apply_brush(&mut raised, Vec2::ZERO));
        state.brush_mode = TerrainBrushMode::Lower;
        assert!(state.apply_brush(&mut lowered, Vec2::ZERO));
        assert_eq!(raised.heights()[40], -lowered.heights()[40]);
    }

    #[test]
    fn scatter_preview_is_deterministic_and_sanitized() {
        let state = TerrainToolState {
            scatter_density: f32::NAN,
            ..TerrainToolState::default()
        };
        let area = ScatterArea::new(Vec2::ZERO, Vec2::splat(8.0));
        let a = state.scatter_preview(area, None).unwrap();
        let b = state.scatter_preview(area, None).unwrap();
        assert_eq!(a, b);
        assert!(a.len() <= 16);
    }

    #[test]
    fn invalid_controls_are_repaired() {
        let mut state = TerrainToolState {
            radius: f32::INFINITY,
            scatter_spacing: -1.0,
            ..TerrainToolState::default()
        };
        state.sanitize();
        assert!(state.radius.is_finite() && state.radius > 0.0);
        assert!(state.scatter_spacing > 0.0);
    }

    #[test]
    fn terrain_asset_round_trips_and_rejects_bad_dimensions() {
        let map = Heightmap::from_fn(5, 4.0, |row, col| row as f32 + col as f32).unwrap();
        let state = TerrainToolState::default();
        let asset = TerrainAsset::from_heightmap(&map, &state);
        let (round_trip, _) = asset.clone().into_parts().unwrap();
        assert_eq!(round_trip, map);
        let bad = TerrainAsset {
            resolution: 1,
            ..asset
        };
        assert!(bad.into_parts().is_err());
    }

    #[test]
    fn terrain_asset_load_rejects_oversized_source_before_parsing() {
        let path = std::env::temp_dir().join(format!(
            "vge-large-terrain-{}-{}.terrain",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(32 * 1024 * 1024 + 1).unwrap();
        let error = TerrainAsset::load(&path).unwrap_err();
        assert!(error.to_string().contains("too large"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn terrain_save_preserves_an_existing_sibling_tmp_file() {
        let root = std::env::temp_dir().join(format!(
            "vge-terrain-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Terrain.terrain");
        let sibling = root.join("Terrain.tmp");
        std::fs::write(&sibling, b"keep this").unwrap();
        let map = Heightmap::new(9, 8.0).unwrap();
        TerrainAsset::from_heightmap(&map, &TerrainToolState::default())
            .save(&path)
            .unwrap();
        assert_eq!(std::fs::read(&sibling).unwrap(), b"keep this");
        assert!(TerrainAsset::load(&path).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn new_terrain_writes_reopenable_heightmap_and_playable_mesh_without_overwriting() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vge-terrain-authoring-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets).unwrap();

        let first = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();
        let saved = TerrainAsset::load(&first.with_extension("terrain")).unwrap();
        let (map, _) = saved.into_parts().unwrap();
        assert!(map.heights().iter().all(|height| *height == 0.0));
        let model = engine_asset::import_gltf_file(&first, &assets).unwrap();
        assert_eq!(model.meshes.len(), 1);
        assert_eq!(model.meshes[0].vertices.len(), map.heights().len());

        let bytes = std::fs::read(&first).unwrap();
        let second = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sculpting_updates_the_editable_heightmap_and_playable_mesh_together() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vge-terrain-sculpt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        let model_path = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();

        let edit = sculpt_terrain(
            &model_path,
            &TerrainToolState::default(),
            &[Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0)],
        )
        .unwrap()
        .expect("stroke changed the terrain");
        let saved = TerrainAsset::load(&model_path.with_extension("terrain")).unwrap();
        assert_eq!(saved, edit.after);
        assert!(edit.before.heights.iter().all(|height| *height == 0.0));
        let (map, _) = saved.into_parts().unwrap();
        assert!(map.height_at(-5.0, 0.0) > 0.0);
        assert!(map.height_at(5.0, 0.0) > 0.0);
        let middle = 32 * 65 + 42;
        let imported = engine_asset::import_gltf_file(&model_path, &assets).unwrap();
        assert_eq!(
            imported.meshes[0].vertices[middle].position[1],
            map.heights()[middle]
        );
        assert!(
            sculpt_terrain(&model_path, &TerrainToolState::default(), &[])
                .unwrap()
                .is_none()
        );

        write_terrain(&model_path, &edit.before).unwrap();
        let restored = TerrainAsset::load(&model_path.with_extension("terrain")).unwrap();
        assert_eq!(restored, edit.before);
        let imported = engine_asset::import_gltf_file(&model_path, &assets).unwrap();
        assert_eq!(imported.meshes[0].vertices[middle].position[1], 0.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn viewport_ray_hits_the_terrain_surface_inside_its_bounds() {
        let map = Heightmap::new(65, 32.0).unwrap();
        assert_eq!(
            raycast_heightmap(&map, Vec3::new(2.0, 10.0, 3.0), Vec3::NEG_Y),
            Some(Vec2::new(2.0, 3.0))
        );
        assert_eq!(
            raycast_heightmap(&map, Vec3::new(20.0, 10.0, 3.0), Vec3::NEG_Y),
            None
        );
        let hit = raycast_heightmap(&map, Vec3::new(0.0, 10.0, 10.0), Vec3::new(0.0, -1.0, -1.0))
            .unwrap();
        assert!(hit.distance(Vec2::ZERO) < 0.01);
    }

    #[test]
    fn center_of_scene_view_projects_to_selected_terrain_local_coordinates() {
        let map = Heightmap::new(65, 32.0).unwrap();
        let camera = engine_renderer::Camera::new(Vec3::new(0.0, 10.0, 10.0), Vec3::ZERO, 1.0);
        let hit = terrain_hit_from_screen(
            camera.view_projection_matrix(),
            glam::Mat4::IDENTITY,
            &map,
            Vec2::splat(50.0),
            Vec2::splat(100.0),
        )
        .unwrap();
        assert!(hit.distance(Vec2::ZERO) < 0.01);
        assert!(
            terrain_hit_from_screen(
                camera.view_projection_matrix(),
                glam::Mat4::IDENTITY,
                &map,
                Vec2::ZERO,
                Vec2::ZERO,
            )
            .is_none()
        );
    }

    #[test]
    fn fast_pointer_motion_fills_a_bounded_continuous_brush_stroke() {
        let mut stroke = TerrainStroke {
            entity: engine_ecs::prelude::Entity::PLACEHOLDER,
            relative_model: PathBuf::from("terrain/Terrain.gltf"),
            heightmap: Heightmap::new(65, 32.0).unwrap(),
            centers: Vec::new(),
        };
        stroke.record(Vec2::new(-4.0, 0.0), 1.0);
        stroke.record(Vec2::new(4.0, 0.0), 1.0);
        assert!(stroke.centers.len() >= 9);
        assert_eq!(stroke.centers.last(), Some(&Vec2::new(4.0, 0.0)));
        for _ in 0..2000 {
            stroke.record(Vec2::new(4.0, 0.0), 0.1);
        }
        assert!(stroke.centers.len() <= 1024);
    }
}
