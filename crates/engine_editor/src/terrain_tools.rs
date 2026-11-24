//! Terrain authoring: brush settings, strokes, undo edits, new terrains and
//! tiles, and the Terrain menu controls.
//!
//! A terrain's heights and paint live in its tile files (see
//! [`engine_scene::TerrainData`]). Studio keeps a live copy of each
//! terrain ([`crate::terrain_edit::LiveTerrains`]); a stroke edits that
//! copy as the pointer moves, the Scene View re-uploads only the chunks it
//! touched, and when the stroke ends only the touched tiles are saved and
//! one [`TerrainEdit`] goes on the undo stack.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use engine_renderer::{
    Brush, BrushFalloff, BuiltinLayer, DirtyRect, Heightmap, MAX_TERRAIN_LAYERS, ScatterArea,
    ScatterConfig, TerrainSurface, TerrainTile, TileCoord, scatter,
};
pub use engine_scene::{TerrainAsset, TerrainToolStateData};
use engine_scene::{TerrainData, TerrainLayerData};
use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::theme::NIGHT;

/// Nodes per side of a new terrain tile.
pub const NEW_TILE_RESOLUTION: u32 = 129;
/// Edge length of a new terrain tile, in metres.
pub const NEW_TILE_SIZE: f32 = 64.0;

/// One viewport gesture on one terrain.
#[derive(Debug)]
pub struct TerrainStroke {
    /// The terrain object.
    pub entity: engine_ecs::prelude::Entity,
    /// Brush centres in terrain-local XZ, densely sampled along the drag.
    pub centers: Vec<Vec2>,
    /// How many of `centers` have been applied to the live terrain.
    pub applied: usize,
    /// Each touched tile as it was before the stroke.
    pub before: BTreeMap<TileCoord, TerrainTile>,
}

impl TerrainStroke {
    /// A stroke on `entity` that hasn't touched anything yet.
    pub fn new(entity: engine_ecs::prelude::Entity) -> Self {
        Self {
            entity,
            centers: Vec::new(),
            applied: 0,
            before: BTreeMap::new(),
        }
    }

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

/// The operation performed by a sculpt brush.
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

impl TerrainBrushMode {
    /// Every mode, in menu order.
    pub const ALL: [TerrainBrushMode; 4] = [
        TerrainBrushMode::Raise,
        TerrainBrushMode::Lower,
        TerrainBrushMode::Smooth,
        TerrainBrushMode::Flatten,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            TerrainBrushMode::Raise => "Raise",
            TerrainBrushMode::Lower => "Lower",
            TerrainBrushMode::Smooth => "Smooth",
            TerrainBrushMode::Flatten => "Flatten",
        }
    }
}

/// Persistent editor controls for terrain sculpting, painting and
/// vegetation previews.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainToolState {
    /// Paint layers instead of sculpting.
    pub paint: bool,
    /// Active sculpt operation.
    pub brush_mode: TerrainBrushMode,
    /// The layer painted in paint mode.
    pub paint_layer: usize,
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

impl Default for TerrainToolState {
    fn default() -> Self {
        Self {
            paint: false,
            brush_mode: TerrainBrushMode::Raise,
            paint_layer: 0,
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
        self.paint_layer = self.paint_layer.min(MAX_TERRAIN_LAYERS - 1);
    }

    /// The scatter controls saved with a tile.
    pub fn tools_data(&self) -> TerrainToolStateData {
        TerrainToolStateData {
            scatter_density: self.scatter_density,
            scatter_spacing: self.scatter_spacing,
            scatter_seed: self.scatter_seed,
        }
    }

    /// Applies one brush dab at terrain-local `center`. Returns the nodes
    /// it changed, or `None` if nothing changed.
    pub fn apply_brush(&self, surface: &mut TerrainSurface, center: Vec2) -> Option<DirtyRect> {
        let mut state = self.clone();
        state.sanitize();
        let brush = Brush {
            center,
            radius: state.radius,
            strength: state.strength,
            falloff: state.falloff,
        };
        if state.paint {
            return surface.paint(
                &Brush {
                    strength: brush.strength.abs().min(1.0),
                    ..brush
                },
                state.paint_layer,
            );
        }
        match state.brush_mode {
            TerrainBrushMode::Raise => surface.raise_lower(&brush),
            TerrainBrushMode::Lower => surface.raise_lower(&Brush {
                strength: -brush.strength.abs(),
                ..brush
            }),
            TerrainBrushMode::Smooth => surface.smooth(&brush),
            TerrainBrushMode::Flatten => surface.flatten(&brush, state.flatten_target),
        }
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

/// One tile changed by a stroke, before and after.
#[derive(Debug, Clone, PartialEq)]
pub struct TileEdit {
    /// The tile file.
    pub path: PathBuf,
    /// The tile before the stroke.
    pub before: TerrainTile,
    /// The tile after the stroke.
    pub after: TerrainTile,
    /// Scatter controls saved with it.
    pub tools: TerrainToolStateData,
}

/// One stroke: every tile it changed, for undo/redo.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainEdit {
    /// The tiles.
    pub tiles: Vec<TileEdit>,
}

impl TerrainEdit {
    /// Approximate memory held by this edit, for bounding the undo history.
    pub fn byte_size(&self) -> usize {
        self.tiles
            .iter()
            .map(|tile| {
                let nodes =
                    tile.before.heights.heights().len() + tile.after.heights.heights().len();
                nodes * (std::mem::size_of::<f32>() + MAX_TERRAIN_LAYERS)
            })
            .sum()
    }

    /// The tile files this edit writes.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.tiles.iter().map(|tile| tile.path.clone()).collect()
    }

    /// Writes every tile's `before` (undo) or `after` (redo) state.
    ///
    /// # Errors
    ///
    /// The first I/O error; tiles before it are already written.
    pub fn write(&self, undo: bool) -> io::Result<()> {
        for tile in &self.tiles {
            let target = if undo { &tile.before } else { &tile.after };
            engine_scene::save_tile_file(&tile.path, target, &tile.tools)?;
        }
        Ok(())
    }
}

/// Writes a new tile file (and its paint) without replacing anything.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] if the file exists; any write error.
pub fn write_new_tile(
    path: &Path,
    tile: &TerrainTile,
    tools: &TerrainToolStateData,
) -> io::Result<()> {
    let text = ron::ser::to_string_pretty(
        &TerrainAsset::from_heightmap(&tile.heights, tools.clone()),
        ron::ser::PrettyConfig::default(),
    )
    .map_err(io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(error) = io::Write::write_all(&mut file, text.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    drop(file);
    // The paint files sit beside it; a new tile is all layer 0, which is
    // what a missing paint file means, but writing it keeps the pair
    // together from the start.
    let [paint, _] = engine_scene::splat_paths(path);
    if !paint.exists() {
        let rgba = tile.splat.rgba(0);
        let resolution = tile.splat.resolution();
        let png = engine_renderer::encode_rgba8_png(resolution, resolution, &rgba)
            .map_err(io::Error::other)?;
        engine_scene::write_atomic(&paint, &png)?;
    }
    Ok(())
}

/// Creates a new terrain folder `assets/terrain/<Name>/` with one flat tile
/// and returns the tile file's path. Never replaces existing files.
///
/// # Errors
///
/// Any I/O error, or [`io::ErrorKind::AlreadyExists`] after 10,000 names.
pub fn write_new_terrain(assets_dir: &Path, state: &TerrainToolState) -> io::Result<PathBuf> {
    let root = assets_dir.join("terrain");
    std::fs::create_dir_all(&root)?;
    let tile = TerrainTile::flat(NEW_TILE_RESOLUTION, NEW_TILE_SIZE).map_err(io::Error::other)?;
    for index in 0..10_000 {
        let name = if index == 0 {
            "Terrain".to_owned()
        } else {
            format!("Terrain {index}")
        };
        let folder = root.join(&name);
        // Old single-file terrains used `terrain/<Name>.terrain`; skip
        // those names too so the two never look alike.
        if folder.exists() || root.join(format!("{name}.terrain")).exists() {
            continue;
        }
        match std::fs::create_dir(&folder) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        let path = folder.join(tile_file_name((0, 0)));
        write_new_tile(&path, &tile, &state.tools_data())?;
        return Ok(path);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many terrains already exist",
    ))
}

/// `tile_<x>_<z>.terrain`.
pub fn tile_file_name(coord: TileCoord) -> String {
    format!("tile_{}_{}.terrain", coord.0, coord.1)
}

/// A tile to add or remove, queued by the Terrain menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainTileRequest {
    /// A new tile on `side` of tile `from`.
    Add {
        /// The terrain object.
        entity: engine_ecs::prelude::Entity,
        /// The tile it goes next to.
        from: TileCoord,
        /// Which side.
        side: TileSide,
    },
    /// Remove the tile at `coord` (its file stays on disk).
    Remove {
        /// The terrain object.
        entity: engine_ecs::prelude::Entity,
        /// The tile.
        coord: TileCoord,
    },
}

/// A side of a tile, for adding a neighbour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSide {
    /// −Z.
    North,
    /// +X.
    East,
    /// +Z.
    South,
    /// −X.
    West,
}

impl TileSide {
    /// Every side, in menu order.
    pub const ALL: [TileSide; 4] = [
        TileSide::North,
        TileSide::East,
        TileSide::South,
        TileSide::West,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            TileSide::North => "North",
            TileSide::East => "East",
            TileSide::South => "South",
            TileSide::West => "West",
        }
    }

    /// The neighbouring coordinate on this side.
    pub fn of(self, coord: TileCoord) -> TileCoord {
        match self {
            TileSide::North => (coord.0, coord.1 - 1),
            TileSide::East => (coord.0 + 1, coord.1),
            TileSide::South => (coord.0, coord.1 + 1),
            TileSide::West => (coord.0 - 1, coord.1),
        }
    }
}

/// A new tile for `coord`: edges taken from the neighbours already there,
/// the inside blended between its four sides (a side with no neighbour
/// counts as flat ground at 0).
pub fn new_neighbour_tile(
    surface: &TerrainSurface,
    coord: TileCoord,
) -> Result<TerrainTile, String> {
    let resolution = surface.resolution();
    let size = surface.size();
    let n = i64::from(resolution) - 1;
    let (row0, col0) = surface.tile_origin(coord);
    let edge = |row: i64, col: i64| surface.node_height(row0 + row, col0 + col);
    let heights = Heightmap::from_fn(resolution, size, |row, col| {
        let (row, col) = (i64::from(row), i64::from(col));
        // Nodes shared with a neighbour keep its value exactly.
        if let Some(shared) = edge(row, col) {
            return shared;
        }
        // Inverse-distance blend of the four edge values on this row and
        // column; a side with no neighbour counts as flat ground at 0, so
        // a hill at the shared edge fades out across the new tile.
        let samples = [
            (edge(0, col), row as f32),
            (edge(n, col), (n - row) as f32),
            (edge(row, 0), col as f32),
            (edge(row, n), (n - col) as f32),
        ];
        let mut total = 0.0;
        let mut weight = 0.0;
        for (value, distance) in samples {
            let w = 1.0 / (distance + 1.0).powi(2);
            total += value.unwrap_or(0.0) * w;
            weight += w;
        }
        total / weight
    })
    .map_err(|error| error.to_string())?;
    Ok(TerrainTile {
        heights,
        splat: engine_renderer::SplatMap::new(resolution),
    })
}

/// Maps a Scene View pointer position to the first hit on a transformed
/// terrain, in terrain-local XZ. `pointer` and `size` are in viewport-local
/// logical pixels.
pub fn terrain_hit_from_screen(
    view_projection: Mat4,
    model: Mat4,
    surface: &TerrainSurface,
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
    surface.raycast(origin, direction)
}

/// Points on a circle of `radius` around terrain-local `center`, draped on
/// the ground, for the brush ring.
pub fn brush_ring(surface: &TerrainSurface, center: Vec2, radius: f32) -> Vec<Vec3> {
    const SEGMENTS: usize = 48;
    (0..=SEGMENTS)
        .filter_map(|index| {
            let angle = index as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let point = center + Vec2::new(angle.cos(), angle.sin()) * radius;
            let height = surface.height_at(point.x, point.y)?;
            Some(Vec3::new(point.x, height + 0.05, point.y))
        })
        .collect()
}

/// What the Terrain controls asked for this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ControlsResponse {
    /// The brush was switched on or off, or between sculpt and paint.
    pub mode_changed: bool,
    /// A layer setting changed.
    pub layers_changed: bool,
}

/// The Off / Sculpt / Paint switch, brush settings and, when painting, the
/// selected terrain's layer list. `layers` is the selected terrain, if any;
/// `textures` lists the project's images as `(id, name)`.
pub fn show_controls(
    ui: &mut egui::Ui,
    state: &mut TerrainToolState,
    active: &mut bool,
    terrain: Option<&mut TerrainData>,
    textures: &[(String, String)],
) -> ControlsResponse {
    state.sanitize();
    let mut response = ControlsResponse::default();
    crate::widgets::segmented(&NIGHT).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, on, paint) in [
                ("Off", false, false),
                ("Sculpt", true, false),
                ("Paint", true, true),
            ] {
                let selected = *active == on && (!on || state.paint == paint);
                if crate::widgets::segment_label(ui, &NIGHT, selected, label).clicked() && !selected
                {
                    *active = on;
                    state.paint = paint;
                    response.mode_changed = true;
                }
            }
        });
    });
    if state.paint {
        match terrain {
            Some(terrain) => {
                ui.label(egui::RichText::new("Layers").color(NIGHT.muted));
                response.layers_changed |=
                    layer_list(ui, terrain, textures, Some(&mut state.paint_layer));
            }
            None => {
                ui.label(
                    egui::RichText::new("Select a terrain to see its layers").color(NIGHT.subtle),
                );
            }
        }
    } else {
        crate::widgets::segmented(&NIGHT).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for mode in TerrainBrushMode::ALL {
                    let selected = state.brush_mode == mode;
                    if crate::widgets::segment_label(ui, &NIGHT, selected, mode.label()).clicked() {
                        state.brush_mode = mode;
                    }
                }
            });
        });
    }
    ui.add(egui::Slider::new(&mut state.radius, 0.01..=100.0).text("Radius"));
    if state.paint {
        ui.add(egui::Slider::new(&mut state.strength, 0.0..=1.0).text("Strength"));
    } else {
        ui.add(egui::Slider::new(&mut state.strength, -10.0..=10.0).text("Strength"));
        if state.brush_mode == TerrainBrushMode::Flatten {
            ui.add(egui::DragValue::new(&mut state.flatten_target).prefix("Target "));
        }
    }
    ui.separator();
    ui.label(egui::RichText::new("Foliage scatter").color(NIGHT.muted));
    ui.add(egui::Slider::new(&mut state.scatter_density, 0.0..=1.0).text("Density"));
    ui.add(
        egui::DragValue::new(&mut state.scatter_spacing)
            .prefix("Spacing ")
            .range(0.01..=10000.0),
    );
    ui.add(egui::DragValue::new(&mut state.scatter_seed).prefix("Seed "));
    response
}

fn swatch(ui: &mut egui::Ui, colour: [f32; 3]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let colour = egui::Rgba::from_rgb(colour[0], colour[1], colour[2]);
    ui.painter().rect_filled(rect, 4.0, colour);
}

/// One row per paint layer (swatch, name, texture, tint, tiling) with Add
/// and Remove. With `selected`, clicking a row picks the layer to paint.
/// Returns whether anything changed.
pub fn layer_list(
    ui: &mut egui::Ui,
    terrain: &mut TerrainData,
    textures: &[(String, String)],
    mut selected: Option<&mut usize>,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    let count = terrain.layers.len();
    for (index, layer) in terrain.layers.iter_mut().enumerate() {
        ui.push_id(("terrain layer", index), |ui| {
            ui.horizontal(|ui| {
                let tint = layer.tint;
                let base = layer.builtin.swatch();
                swatch(
                    ui,
                    [base[0] * tint[0], base[1] * tint[1], base[2] * tint[2]],
                );
                let picked = selected.as_deref().is_some_and(|chosen| *chosen == index);
                let name = egui::RichText::new(&layer.name).color(if picked {
                    NIGHT.text
                } else {
                    NIGHT.muted
                });
                if let Some(chosen) = selected.as_deref_mut() {
                    if ui.selectable_label(picked, name).clicked() {
                        *chosen = index;
                    }
                } else {
                    ui.label(name);
                }
                if count > 1
                    && crate::widgets::pill_button(ui, &NIGHT, "Remove", NIGHT.danger, true)
                        .clicked()
                {
                    remove = Some(index);
                }
            });
            changed |= layer_settings(ui, layer, textures);
        });
    }
    if let Some(index) = remove {
        terrain.layers.remove(index);
        if let Some(chosen) = selected {
            *chosen = (*chosen).min(terrain.layers.len().saturating_sub(1));
        }
        changed = true;
    }
    if crate::widgets::pill_button(
        ui,
        &NIGHT,
        "Add layer",
        NIGHT.accent,
        count < MAX_TERRAIN_LAYERS,
    )
    .clicked()
    {
        let builtin = BuiltinLayer::ALL[count % BuiltinLayer::ALL.len()];
        terrain.layers.push(TerrainLayerData::builtin(builtin));
        changed = true;
    }
    changed
}

fn layer_settings(
    ui: &mut egui::Ui,
    layer: &mut TerrainLayerData,
    textures: &[(String, String)],
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let current = layer
            .texture
            .as_ref()
            .and_then(|texture| textures.iter().find(|(id, _)| *id == texture.id))
            .map_or_else(
                || format!("Built-in {}", layer.builtin.label()),
                |(_, name)| name.clone(),
            );
        egui::ComboBox::from_id_salt("texture")
            .selected_text(current)
            .width(150.0)
            .show_ui(ui, |ui| {
                for builtin in BuiltinLayer::ALL {
                    let chosen = layer.texture.is_none() && layer.builtin == builtin;
                    if ui
                        .selectable_label(chosen, format!("Built-in {}", builtin.label()))
                        .clicked()
                        && !chosen
                    {
                        layer.texture = None;
                        layer.builtin = builtin;
                        changed = true;
                    }
                }
                for (id, name) in textures {
                    let chosen = layer
                        .texture
                        .as_ref()
                        .is_some_and(|texture| texture.id == *id);
                    if ui.selectable_label(chosen, name).clicked() && !chosen {
                        layer.texture = Some(engine_scene::AssetRef { id: id.clone() });
                        changed = true;
                    }
                }
            });
        let mut tint = egui::Rgba::from_rgb(layer.tint[0], layer.tint[1], layer.tint[2]);
        if egui::color_picker::color_edit_button_rgba(
            ui,
            &mut tint,
            egui::color_picker::Alpha::Opaque,
        )
        .changed()
        {
            layer.tint = [tint.r(), tint.g(), tint.b()];
            changed = true;
        }
        changed |= ui
            .add(
                egui::DragValue::new(&mut layer.tiling)
                    .speed(0.1)
                    .range(0.1..=1000.0)
                    .suffix(" m"),
            )
            .on_hover_text("How many metres one copy of the texture covers")
            .changed();
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vge-terrain-tools-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        root
    }

    fn surface(coords: &[TileCoord]) -> TerrainSurface {
        let mut surface = TerrainSurface::new(33, 16.0).unwrap();
        for coord in coords {
            surface
                .insert_tile(*coord, TerrainTile::flat(33, 16.0).unwrap())
                .unwrap();
        }
        surface
    }

    #[test]
    fn lower_is_the_inverse_of_raise() {
        let mut raised = surface(&[(0, 0)]);
        let mut lowered = raised.clone();
        let mut state = TerrainToolState::default();
        assert!(state.apply_brush(&mut raised, Vec2::ZERO).is_some());
        state.brush_mode = TerrainBrushMode::Lower;
        assert!(state.apply_brush(&mut lowered, Vec2::ZERO).is_some());
        assert_eq!(
            raised.node_height(16, 16),
            lowered.node_height(16, 16).map(|h| -h)
        );
    }

    #[test]
    fn paint_mode_changes_paint_only() {
        let mut ground = surface(&[(0, 0)]);
        let state = TerrainToolState {
            paint: true,
            paint_layer: 2,
            strength: 0.5,
            ..TerrainToolState::default()
        };
        assert!(state.apply_brush(&mut ground, Vec2::ZERO).is_some());
        assert_eq!(ground.node_height(16, 16), Some(0.0));
        assert!(ground.node_weights(16, 16).unwrap()[2] > 0);
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
            paint_layer: 99,
            ..TerrainToolState::default()
        };
        state.sanitize();
        assert!(state.radius.is_finite() && state.radius > 0.0);
        assert!(state.scatter_spacing > 0.0);
        assert_eq!(state.paint_layer, MAX_TERRAIN_LAYERS - 1);
    }

    #[test]
    fn new_terrain_writes_a_tile_folder_and_never_overwrites() {
        let root = scratch("new");
        let assets = root.join("assets");
        let first = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();
        assert!(first.ends_with("terrain/Terrain/tile_0_0.terrain"));
        let (tile, _) = engine_scene::load_tile_file(&first).unwrap();
        assert_eq!(tile.heights.resolution(), NEW_TILE_RESOLUTION);
        assert!(tile.heights.heights().iter().all(|h| *h == 0.0));
        assert!(engine_scene::splat_paths(&first)[0].exists());
        // No model file any more.
        assert!(
            std::fs::read_dir(first.parent().unwrap())
                .unwrap()
                .all(|entry| entry
                    .unwrap()
                    .path()
                    .extension()
                    .is_none_or(|e| e != "gltf"))
        );
        let bytes = std::fs::read(&first).unwrap();
        // An old single-file terrain's name is skipped too.
        std::fs::write(assets.join("terrain/Terrain 1.terrain"), b"old").unwrap();
        let second = write_new_terrain(&assets, &TerrainToolState::default()).unwrap();
        assert!(second.ends_with("terrain/Terrain 2/tile_0_0.terrain"));
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        assert!(write_new_tile(&first, &tile, &TerrainToolStateData::default()).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_edit_writes_before_on_undo_and_after_on_redo() {
        let root = scratch("edit");
        let path = root.join("assets/tile_0_0.terrain");
        let mut ground = surface(&[(0, 0)]);
        let before = ground.tile((0, 0)).unwrap().clone();
        TerrainToolState::default().apply_brush(&mut ground, Vec2::ZERO);
        let after = ground.tile((0, 0)).unwrap().clone();
        let edit = TerrainEdit {
            tiles: vec![TileEdit {
                path: path.clone(),
                before: before.clone(),
                after: after.clone(),
                tools: TerrainToolStateData::default(),
            }],
        };
        assert!(edit.byte_size() > 0);
        edit.write(false).unwrap();
        assert_eq!(engine_scene::load_tile_file(&path).unwrap().0, after);
        edit.write(true).unwrap();
        assert_eq!(engine_scene::load_tile_file(&path).unwrap().0, before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_neighbour_tile_shares_its_edge_and_blends_inside() {
        let mut ground = surface(&[(0, 0)]);
        ground.raise_lower(&Brush::new(Vec2::new(8.0, 0.0), 3.0, 2.0));
        let tile = new_neighbour_tile(&ground, (1, 0)).unwrap();
        for row in 0..33 {
            assert_eq!(
                tile.heights.node_height(row, 0),
                ground.node_height(row, 32).unwrap()
            );
        }
        // Inside, the bump fades away from the shared edge.
        assert!(tile.heights.node_height(16, 1) > tile.heights.node_height(16, 20));
        ground.insert_tile((1, 0), tile).unwrap();
        assert_eq!(TileSide::East.of((0, 0)), (1, 0));
        assert_eq!(TileSide::North.of((0, 0)), (0, -1));
    }

    #[test]
    fn viewport_ray_hits_the_terrain_and_misses_outside() {
        let ground = surface(&[(0, 0)]);
        let camera = engine_renderer::Camera::new(Vec3::new(0.0, 10.0, 10.0), Vec3::ZERO, 1.0);
        let hit = terrain_hit_from_screen(
            camera.view_projection_matrix(),
            Mat4::IDENTITY,
            &ground,
            Vec2::splat(50.0),
            Vec2::splat(100.0),
        )
        .unwrap();
        assert!(hit.distance(Vec2::ZERO) < 0.01);
        assert!(
            terrain_hit_from_screen(
                camera.view_projection_matrix(),
                Mat4::IDENTITY,
                &ground,
                Vec2::ZERO,
                Vec2::ZERO,
            )
            .is_none()
        );
        let ring = brush_ring(&ground, Vec2::ZERO, 2.0);
        assert_eq!(ring.len(), 49);
        assert!(brush_ring(&ground, Vec2::splat(100.0), 1.0).is_empty());
    }

    #[test]
    fn fast_pointer_motion_fills_a_bounded_continuous_brush_stroke() {
        let mut stroke = TerrainStroke::new(engine_ecs::prelude::Entity::PLACEHOLDER);
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
