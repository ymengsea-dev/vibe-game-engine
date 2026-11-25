//! Material files (`.material`) and how an object's material is put
//! together: the model's own material, or a material file in its place,
//! then the object's own changes, each map's image coming from the model
//! or from an image asset.

use std::path::Path;

use engine_asset::{ImportedGltf, ImportedMaterial};
use engine_ecs::components::{MaterialImages, RgbaImage};
use engine_renderer::{AlphaMode, Material};
use serde::{Deserialize, Serialize};

use crate::SceneError;
use crate::format::{AssetRef, MaterialOverrideData};

/// The file extension of a material file.
pub const MATERIAL_EXTENSION: &str = "material";

/// One image per map a material can carry, each an image asset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialImageRefs {
    /// Base colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color: Option<AssetRef>,
    /// Bumps: a tangent-space normal map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<AssetRef>,
    /// Glow colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive: Option<AssetRef>,
    /// Ambient shadow in crevices, in red.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion: Option<AssetRef>,
    /// Roughness in green and metallic in blue, as glTF lays them out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic_roughness: Option<AssetRef>,
}

impl MaterialImageRefs {
    /// Whether no map has an image.
    pub fn is_empty(&self) -> bool {
        self.iter().all(|image| image.is_none())
    }

    /// The five images, base colour first.
    pub fn iter(&self) -> impl Iterator<Item = Option<&AssetRef>> {
        [
            self.base_color.as_ref(),
            self.normal.as_ref(),
            self.emissive.as_ref(),
            self.occlusion.as_ref(),
            self.metallic_roughness.as_ref(),
        ]
        .into_iter()
    }

    fn validate(&self) -> Result<(), String> {
        for image in self.iter().flatten() {
            image.validate()?;
        }
        Ok(())
    }
}

/// How a material treats its base colour's alpha.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum MaterialAlpha {
    /// Alpha is ignored.
    #[default]
    Opaque,
    /// Pixels with alpha below `threshold` are cut away; the rest are
    /// solid. For leaves, fences and grass.
    Cutout {
        /// The alpha below which a pixel is cut, `0` to `1`.
        threshold: f32,
    },
    /// See-through by alpha. For glass and water; slower than cutout.
    Blend,
}

/// A material file: how a surface looks, shared by every object using it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialData {
    /// Base colour (linear RGBA), multiplying the base colour image.
    #[serde(default = "white")]
    pub base_color: [f32; 4],
    /// `0` is plastic, wood or stone; `1` is metal. Multiplies the
    /// metallic-roughness image's blue channel.
    #[serde(default)]
    pub metallic: f32,
    /// `0` is mirror-smooth; `1` is fully rough. Multiplies the
    /// metallic-roughness image's green channel.
    #[serde(default = "half")]
    pub roughness: f32,
    /// How strongly the normal image bends the light, `0` to `2`.
    #[serde(default = "one")]
    pub normal_strength: f32,
    /// Glow colour (linear RGB), multiplying the emissive image. Values
    /// above `1` glow brighter.
    #[serde(default)]
    pub emissive: [f32; 3],
    /// How many times the images repeat across the surface.
    #[serde(default = "one_one")]
    pub uv_scale: [f32; 2],
    /// How far the images are shifted, in image widths.
    #[serde(default)]
    pub uv_offset: [f32; 2],
    /// How alpha is treated.
    #[serde(default)]
    pub alpha: MaterialAlpha,
    /// Draw both faces of each triangle.
    #[serde(default)]
    pub two_sided: bool,
    /// The images.
    #[serde(default, skip_serializing_if = "MaterialImageRefs::is_empty")]
    pub images: MaterialImageRefs,
}

fn white() -> [f32; 4] {
    [1.0; 4]
}
fn half() -> f32 {
    0.5
}
fn one() -> f32 {
    1.0
}
fn one_one() -> [f32; 2] {
    [1.0, 1.0]
}

impl Default for MaterialData {
    /// Plain white, not metal, half rough.
    fn default() -> Self {
        Self {
            base_color: white(),
            metallic: 0.0,
            roughness: half(),
            normal_strength: one(),
            emissive: [0.0; 3],
            uv_scale: one_one(),
            uv_offset: [0.0; 2],
            alpha: MaterialAlpha::Opaque,
            two_sided: false,
            images: MaterialImageRefs::default(),
        }
    }
}

/// The largest glow a material may have.
const MAX_EMISSIVE: f32 = 100.0;
/// The largest texture repeat or shift.
const MAX_UV: f32 = 10_000.0;

impl MaterialData {
    /// Rejects values the renderer cannot use.
    ///
    /// # Errors
    ///
    /// A message naming the bad setting.
    pub fn validate(&self) -> Result<(), String> {
        let unit = |value: f32| value.is_finite() && (0.0..=1.0).contains(&value);
        if !self.base_color.iter().copied().all(unit) {
            return Err("base colour must be between 0 and 1".into());
        }
        if !unit(self.metallic) || !unit(self.roughness) {
            return Err("metallic and roughness must be between 0 and 1".into());
        }
        if !(self.normal_strength.is_finite() && (0.0..=2.0).contains(&self.normal_strength)) {
            return Err("normal strength must be between 0 and 2".into());
        }
        if !self
            .emissive
            .iter()
            .all(|value| value.is_finite() && (0.0..=MAX_EMISSIVE).contains(value))
        {
            return Err(format!("glow must be between 0 and {MAX_EMISSIVE}"));
        }
        if !self
            .uv_scale
            .iter()
            .all(|value| value.is_finite() && value.abs() <= MAX_UV && value.abs() >= 1e-3)
            || !self
                .uv_offset
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_UV)
        {
            return Err("texture repeat and shift are out of range".into());
        }
        if let MaterialAlpha::Cutout { threshold } = self.alpha
            && !unit(threshold)
        {
            return Err("cutout threshold must be between 0 and 1".into());
        }
        self.images.validate()
    }

    /// The renderer material for these settings.
    pub fn to_material(&self) -> Material {
        Material {
            base_color_factor: self.base_color,
            metallic_factor: self.metallic,
            roughness_factor: self.roughness,
            emissive_factor: self.emissive,
            normal_scale: self.normal_strength,
            occlusion_strength: 1.0,
            alpha_mode: match self.alpha {
                MaterialAlpha::Opaque => AlphaMode::Opaque,
                MaterialAlpha::Cutout { threshold } => AlphaMode::Mask { cutoff: threshold },
                MaterialAlpha::Blend => AlphaMode::Blend,
            },
            double_sided: self.two_sided,
            uv_scale: self.uv_scale,
            uv_offset: self.uv_offset,
        }
    }

    /// Parses and checks a material file's text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] for text that is not a valid material.
    pub fn from_ron(text: &str) -> Result<Self, SceneError> {
        let data: Self = ron::from_str(text)
            .map_err(|error| SceneError::Resolve(format!("material file: {error}")))?;
        data.validate()
            .map_err(|reason| SceneError::Resolve(format!("material file: {reason}")))?;
        Ok(data)
    }

    /// The file's text.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if it cannot be written as RON.
    pub fn to_ron(&self) -> Result<String, SceneError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|error| SceneError::Resolve(format!("material file: {error}")))
    }

    /// Reads a material file.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if it cannot be read or is not a valid
    /// material.
    pub fn load(path: &Path) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| SceneError::Resolve(format!("{}: {error}", path.display())))?;
        Self::from_ron(&text)
    }

    /// Writes a material file in one step, so a crash never leaves half a
    /// file.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if it is not valid or cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), SceneError> {
        self.validate()
            .map_err(|reason| SceneError::Resolve(format!("material file: {reason}")))?;
        crate::io::write_atomic(path, self.to_ron()?.as_bytes())
            .map_err(|error| SceneError::Resolve(format!("{}: {error}", path.display())))
    }
}

/// Where one map's image comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapSource {
    /// An image inside the model file, by index.
    Model(usize),
    /// An image asset.
    Asset(AssetRef),
}

/// Where each map's image comes from; `None` for no image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterialMapSources {
    /// Base colour.
    pub base_color: Option<MapSource>,
    /// Normal map.
    pub normal: Option<MapSource>,
    /// Glow.
    pub emissive: Option<MapSource>,
    /// Ambient occlusion.
    pub occlusion: Option<MapSource>,
    /// Metallic-roughness.
    pub metallic_roughness: Option<MapSource>,
}

impl MaterialMapSources {
    fn from_model(imported: &ImportedMaterial) -> Self {
        Self {
            base_color: imported.base_color_image.map(MapSource::Model),
            normal: imported.normal_image.map(MapSource::Model),
            emissive: imported.emissive_image.map(MapSource::Model),
            occlusion: imported.occlusion_image.map(MapSource::Model),
            metallic_roughness: imported.metallic_roughness_image.map(MapSource::Model),
        }
    }

    fn from_assets(images: &MaterialImageRefs) -> Self {
        let asset = |image: &Option<AssetRef>| image.clone().map(MapSource::Asset);
        Self {
            base_color: asset(&images.base_color),
            normal: asset(&images.normal),
            emissive: asset(&images.emissive),
            occlusion: asset(&images.occlusion),
            metallic_roughness: asset(&images.metallic_roughness),
        }
    }

    /// Replaces each map that `images` gives an image for.
    fn replace(&mut self, images: &MaterialImageRefs) {
        let replace = |slot: &mut Option<MapSource>, image: &Option<AssetRef>| {
            if let Some(image) = image {
                *slot = Some(MapSource::Asset(image.clone()));
            }
        };
        replace(&mut self.base_color, &images.base_color);
        replace(&mut self.normal, &images.normal);
        replace(&mut self.emissive, &images.emissive);
        replace(&mut self.occlusion, &images.occlusion);
        replace(&mut self.metallic_roughness, &images.metallic_roughness);
    }

    /// Every image asset used.
    pub fn assets(&self) -> impl Iterator<Item = &AssetRef> {
        [
            &self.base_color,
            &self.normal,
            &self.emissive,
            &self.occlusion,
            &self.metallic_roughness,
        ]
        .into_iter()
        .filter_map(|source| match source {
            Some(MapSource::Asset(image)) => Some(image),
            _ => None,
        })
    }

    /// The images themselves: model images from `gltf`, image assets
    /// through `asset`. A source that cannot be found is left out.
    pub fn images<'a>(
        &self,
        gltf: Option<&'a ImportedGltf>,
        asset: impl Fn(&AssetRef) -> Option<RgbaImage<'a>>,
    ) -> MaterialImages<'a> {
        let image = |source: &Option<MapSource>| match source.as_ref()? {
            MapSource::Model(index) => gltf?
                .images
                .get(*index)
                .map(|image| (image.width, image.height, image.rgba8.as_slice())),
            MapSource::Asset(reference) => asset(reference),
        };
        MaterialImages {
            base_color: image(&self.base_color),
            normal: image(&self.normal),
            emissive: image(&self.emissive),
            occlusion: image(&self.occlusion),
            metallic_roughness: image(&self.metallic_roughness),
        }
    }
}

/// What an object draws with: its model's material (`imported`), or the
/// material file `file` in its place, then the object's own changes
/// (`changes`): factors on top, and its images replacing the material's,
/// map by map.
pub fn compose_material(
    imported: Option<&ImportedMaterial>,
    file: Option<&MaterialData>,
    changes: Option<&MaterialOverrideData>,
) -> (Material, MaterialMapSources) {
    let (material, mut sources) = match (file, imported) {
        (Some(file), _) => (
            file.to_material(),
            MaterialMapSources::from_assets(&file.images),
        ),
        (None, Some(imported)) => (
            crate::runtime::material_from_imported(imported),
            MaterialMapSources::from_model(imported),
        ),
        (None, None) => (Material::DEFAULT, MaterialMapSources::default()),
    };
    let Some(changes) = changes else {
        return (material, sources);
    };
    sources.replace(&changes.images);
    (changes.apply(material), sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(id: &str) -> Option<AssetRef> {
        Some(AssetRef { id: id.into() })
    }

    #[test]
    fn a_material_file_round_trips_and_leaves_defaults_out() {
        let mut data = MaterialData {
            base_color: [0.5, 0.25, 1.0, 1.0],
            metallic: 1.0,
            roughness: 0.2,
            uv_scale: [4.0, 2.0],
            alpha: MaterialAlpha::Cutout { threshold: 0.4 },
            two_sided: true,
            ..MaterialData::default()
        };
        data.images.base_color = image("00000000-0000-4000-8000-000000000001");
        data.images.normal = image("00000000-0000-4000-8000-000000000002");
        let text = data.to_ron().expect("writes");
        assert_eq!(MaterialData::from_ron(&text).expect("reads"), data);

        let plain = MaterialData::default().to_ron().expect("writes");
        assert!(!plain.contains("images"), "{plain}");
        assert_eq!(
            MaterialData::from_ron("()").expect("reads"),
            MaterialData::default()
        );
    }

    #[test]
    fn bad_material_files_are_refused() {
        for text in [
            "(roughness: 2.0)",
            "(base_color: (1.0, 1.0, 1.0, -1.0))",
            "(uv_scale: (0.0, 1.0))",
            "(alpha: Cutout(threshold: 1.5))",
            "(images: (base_color: Some((id: \"not an id\"))))",
            "(emissive: (1000.0, 0.0, 0.0))",
        ] {
            assert!(MaterialData::from_ron(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_renderer_material_carries_every_setting() {
        let data = MaterialData {
            base_color: [0.1, 0.2, 0.3, 0.4],
            metallic: 0.7,
            roughness: 0.6,
            normal_strength: 1.5,
            emissive: [2.0, 0.0, 0.0],
            uv_scale: [3.0, 3.0],
            uv_offset: [0.5, 0.0],
            alpha: MaterialAlpha::Blend,
            two_sided: true,
            images: MaterialImageRefs::default(),
        };
        let material = data.to_material();
        assert_eq!(material.base_color_factor, data.base_color);
        assert_eq!(material.metallic_factor, 0.7);
        assert_eq!(material.normal_scale, 1.5);
        assert_eq!(material.alpha_mode, AlphaMode::Blend);
        assert_eq!(material.uv_scale, [3.0, 3.0]);
        assert_eq!(material.uv_offset, [0.5, 0.0]);
        assert!(material.double_sided);
    }

    fn model_material() -> ImportedMaterial {
        ImportedMaterial {
            name: None,
            base_color_factor: [1.0; 4],
            metallic_factor: 0.0,
            roughness_factor: 1.0,
            base_color_image: Some(0),
            emissive_factor: [0.0; 3],
            emissive_image: None,
            normal_image: Some(1),
            normal_scale: 1.0,
            occlusion_image: None,
            occlusion_strength: 1.0,
            metallic_roughness_image: Some(2),
            alpha_mode: "OPAQUE",
            alpha_cutoff: 0.5,
            double_sided: false,
        }
    }

    #[test]
    fn a_material_file_replaces_the_model_material_and_changes_go_on_top() {
        let model = model_material();
        let (_, sources) = compose_material(Some(&model), None, None);
        assert_eq!(sources.base_color, Some(MapSource::Model(0)));
        assert_eq!(sources.metallic_roughness, Some(MapSource::Model(2)));

        let mut file = MaterialData {
            roughness: 0.3,
            ..MaterialData::default()
        };
        file.images.base_color = image("00000000-0000-4000-8000-000000000001");
        let (material, sources) = compose_material(Some(&model), Some(&file), None);
        assert_eq!(material.roughness_factor, 0.3);
        assert_eq!(
            sources.base_color,
            Some(MapSource::Asset(file.images.base_color.clone().unwrap()))
        );
        assert_eq!(sources.normal, None, "the file replaces the model's maps");

        let mut changes = MaterialOverrideData {
            tint: [0.5, 1.0, 1.0],
            ..MaterialOverrideData::default()
        };
        changes.images.normal = image("00000000-0000-4000-8000-000000000003");
        let (material, sources) = compose_material(Some(&model), Some(&file), Some(&changes));
        assert_eq!(material.base_color_factor[0], 0.5);
        assert_eq!(
            sources.normal,
            Some(MapSource::Asset(changes.images.normal.clone().unwrap()))
        );
        assert_eq!(
            sources.base_color,
            Some(MapSource::Asset(file.images.base_color.clone().unwrap()))
        );
        assert_eq!(sources.assets().count(), 2);
    }
}
