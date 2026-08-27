//! PBR material parameters: the metallic-roughness workflow (matches
//! glTF's `pbrMetallicRoughness`, and `engine_asset::ImportedMaterial`'s
//! factors — a glTF-imported material converts straight into one of
//! these).
//!
//! `base_color_factor` is applied now (multiplies the sampled base color
//! texture — real, visible, verifiable without any lighting).
//! `metallic_factor`/`roughness_factor` are plumbed through to the GPU
//! here but not yet consumed by the shader: a physically-based BRDF needs
//! actual light data to do anything with them, and that's the next
//! renderer feature. Faking a metallic/roughness visual effect with no
//! light to reflect would just be a placeholder thrown away next
//! iteration, so this stops at "the data is correctly on the GPU."

/// A metallic-roughness PBR material's parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Base color, multiplied with the sampled base color texture
    /// (linear RGBA, `[0, 1]`).
    pub base_color_factor: [f32; 4],
    /// Metallic factor, `[0, 1]` (`0` = dielectric, `1` = metal).
    pub metallic_factor: f32,
    /// Roughness factor, `[0, 1]` (`0` = mirror-smooth, `1` = fully
    /// rough).
    pub roughness_factor: f32,
}

impl Material {
    /// glTF's own default material: white, fully metallic, fully rough.
    pub const DEFAULT: Self = Self {
        base_color_factor: [1.0, 1.0, 1.0, 1.0],
        metallic_factor: 1.0,
        roughness_factor: 1.0,
    };
}

impl Default for Material {
    /// [`Material::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// GPU-layout material uniform, ready to write into a uniform buffer.
///
/// Padded to 32 bytes (a multiple of 16) to satisfy WGSL uniform address
/// space alignment rules for the struct as a whole, matching
/// [`crate::CameraUniform`]/[`crate::ModelUniform`]'s `Pod`/`Zeroable`
/// pattern.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    /// See [`Material::base_color_factor`].
    pub base_color_factor: [f32; 4],
    /// See [`Material::metallic_factor`].
    pub metallic_factor: f32,
    /// See [`Material::roughness_factor`].
    pub roughness_factor: f32,
    /// Padding only — keeps the struct size a multiple of 16 bytes.
    pub _padding: [f32; 2],
}

impl From<Material> for MaterialUniform {
    fn from(material: Material) -> Self {
        Self {
            base_color_factor: material.base_color_factor,
            metallic_factor: material.metallic_factor,
            roughness_factor: material.roughness_factor,
            _padding: [0.0; 2],
        }
    }
}

impl Default for MaterialUniform {
    /// [`Material::DEFAULT`], converted.
    fn default() -> Self {
        Self::from(Material::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_material_matches_gltf_default() {
        assert_eq!(Material::default(), Material::DEFAULT);
        assert_eq!(Material::DEFAULT.base_color_factor, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(Material::DEFAULT.metallic_factor, 1.0);
        assert_eq!(Material::DEFAULT.roughness_factor, 1.0);
    }

    #[test]
    fn uniform_from_material_preserves_factors() {
        let material = Material {
            base_color_factor: [0.2, 0.4, 0.6, 1.0],
            metallic_factor: 0.3,
            roughness_factor: 0.7,
        };
        let uniform = MaterialUniform::from(material);
        assert_eq!(uniform.base_color_factor, [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(uniform.metallic_factor, 0.3);
        assert_eq!(uniform.roughness_factor, 0.7);
    }

    #[test]
    fn uniform_default_matches_material_default() {
        assert_eq!(
            MaterialUniform::default(),
            MaterialUniform::from(Material::DEFAULT)
        );
    }

    #[test]
    fn uniform_size_is_a_multiple_of_16_bytes() {
        assert_eq!(size_of::<MaterialUniform>() % 16, 0);
        assert_eq!(size_of::<MaterialUniform>(), 32);
    }

    #[test]
    fn uniform_bytes_match_expected_size() {
        let uniform = MaterialUniform::default();
        assert_eq!(bytemuck::bytes_of(&uniform).len(), 32);
    }
}
