//! Bounded terrain and vegetation authoring state.
//!
//! This module deliberately keeps authoring data separate from GPU resources:
//! a caller owns the [`Heightmap`] and applies one bounded operation per user
//! gesture, which makes the operation straightforward to put on the editor's
//! existing undo stack later.

use engine_renderer::{Brush, BrushFalloff, Heightmap, ScatterArea, ScatterConfig, scatter};
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(std::io::Error::other)?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(temp, path)
    }

    /// Loads and validates an asset; malformed or oversized data is rejected.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let text = std::fs::read_to_string(path)?;
        let asset: Self = ron::from_str(&text)?;
        asset.clone().into_parts()?;
        Ok(asset)
    }
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
    ui.label("Vegetation preview");
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
}
