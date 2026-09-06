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

/// How a material's alpha channel is interpreted — glTF 2.0's
/// `alphaMode`, with its `alphaCutoff` folded into the `Mask` variant so
/// a cutoff can only exist where it means something.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    /// Fully opaque; the base colour's alpha is ignored. Renders in the
    /// opaque pass, writing depth.
    Opaque,
    /// Binary cutout: a fragment whose alpha is below `cutoff` is
    /// discarded outright, everything else is fully opaque.
    ///
    /// This is what foliage, fences, grates and hair cards use. It stays
    /// in the opaque pass and still writes depth, so it needs no sorting
    /// — which is why it costs almost nothing compared to
    /// [`AlphaMode::Blend`].
    Mask {
        /// Alpha at or above which the fragment is kept, `[0, 1]`.
        cutoff: f32,
    },
    /// True transparency: the fragment is blended over what is behind it.
    ///
    /// Renders in a separate pass after all opaque geometry, sorted
    /// back-to-front, testing depth but not writing it. Correct for
    /// water, glass and ghosted previews; wrong (and slower) for anything
    /// that could use [`AlphaMode::Mask`] instead.
    Blend,
}

impl AlphaMode {
    /// Whether this mode belongs in the sorted transparent pass.
    pub fn is_transparent(self) -> bool {
        matches!(self, AlphaMode::Blend)
    }

    /// The `(mode, cutoff)` pair the shader reads. `0` opaque, `1` mask,
    /// `2` blend.
    fn to_uniform_parts(self) -> (u32, f32) {
        match self {
            AlphaMode::Opaque => (0, 0.0),
            AlphaMode::Mask { cutoff } => (1, cutoff.clamp(0.0, 1.0)),
            AlphaMode::Blend => (2, 0.0),
        }
    }
}

impl Default for AlphaMode {
    /// [`AlphaMode::Opaque`] — glTF's own default.
    fn default() -> Self {
        Self::Opaque
    }
}

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
    /// Emitted colour, added after lighting — a surface that glows on its
    /// own. Linear RGB, not clamped to `1`: values above are valid for
    /// something meant to bloom.
    pub emissive_factor: [f32; 3],
    /// How strongly the normal map perturbs the surface normal. `0`
    /// ignores it entirely, `1` applies it as authored.
    pub normal_scale: f32,
    /// How strongly the occlusion texture darkens ambient light, `[0, 1]`.
    /// `0` ignores it.
    pub occlusion_strength: f32,
    /// How this material's alpha is interpreted.
    pub alpha_mode: AlphaMode,
}

impl Material {
    /// glTF's own default material: white, fully metallic, fully rough.
    pub const DEFAULT: Self = Self {
        base_color_factor: [1.0, 1.0, 1.0, 1.0],
        metallic_factor: 1.0,
        roughness_factor: 1.0,
        emissive_factor: [0.0, 0.0, 0.0],
        normal_scale: 1.0,
        occlusion_strength: 1.0,
        alpha_mode: AlphaMode::Opaque,
    };

    /// A cutout material for foliage and similar cards, discarding
    /// anything below half alpha.
    pub const FOLIAGE: Self = Self {
        base_color_factor: [1.0, 1.0, 1.0, 1.0],
        metallic_factor: 0.0,
        roughness_factor: 0.9,
        emissive_factor: [0.0, 0.0, 0.0],
        normal_scale: 1.0,
        occlusion_strength: 1.0,
        alpha_mode: AlphaMode::Mask { cutoff: 0.5 },
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
/// Four 16-byte rows, so the struct as a whole satisfies WGSL's uniform
/// address-space alignment rules — matching
/// [`crate::CameraUniform`]/[`crate::ModelUniform`]'s `Pod`/`Zeroable`
/// pattern. Fields are grouped to fill each row rather than in the order
/// [`Material`] declares them; `alpha_mode` and `alpha_cutoff` ride along
/// in the metallic/roughness row instead of claiming one of their own.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    /// See [`Material::base_color_factor`].
    pub base_color_factor: [f32; 4],
    /// See [`Material::metallic_factor`].
    pub metallic_factor: f32,
    /// See [`Material::roughness_factor`].
    pub roughness_factor: f32,
    /// Alpha below which a [`AlphaMode::Mask`] fragment is discarded.
    /// Meaningless unless `alpha_mode` is `1`.
    pub alpha_cutoff: f32,
    /// `0` opaque, `1` mask, `2` blend — see [`AlphaMode`].
    ///
    /// A `u32` rather than an enum because WGSL has no enums; the shader
    /// compares it numerically.
    pub alpha_mode: u32,
    /// See [`Material::emissive_factor`].
    pub emissive_factor: [f32; 3],
    /// See [`Material::normal_scale`].
    pub normal_scale: f32,
    /// See [`Material::occlusion_strength`].
    pub occlusion_strength: f32,
    /// Padding only — completes the final 16-byte row.
    pub _padding: [f32; 3],
}

impl From<Material> for MaterialUniform {
    fn from(material: Material) -> Self {
        let (alpha_mode, alpha_cutoff) = material.alpha_mode.to_uniform_parts();
        Self {
            base_color_factor: material.base_color_factor,
            metallic_factor: material.metallic_factor,
            roughness_factor: material.roughness_factor,
            alpha_cutoff,
            alpha_mode,
            emissive_factor: material.emissive_factor,
            normal_scale: material.normal_scale,
            occlusion_strength: material.occlusion_strength.clamp(0.0, 1.0),
            _padding: [0.0; 3],
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
            ..Material::DEFAULT
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
        // Four 16-byte rows since T-15 added emissive, normal scale,
        // occlusion and the alpha mode/cutoff pair. Was 32.
        assert_eq!(size_of::<MaterialUniform>(), 64);
    }

    #[test]
    fn uniform_bytes_match_expected_size() {
        let uniform = MaterialUniform::default();
        assert_eq!(bytemuck::bytes_of(&uniform).len(), 64);
    }

    // --- T-15: alpha modes and the extended uniform ----------------

    #[test]
    fn alpha_modes_map_to_the_numbers_the_shader_compares() {
        assert_eq!(AlphaMode::Opaque.to_uniform_parts(), (0, 0.0));
        assert_eq!(
            AlphaMode::Mask { cutoff: 0.25 }.to_uniform_parts(),
            (1, 0.25)
        );
        assert_eq!(AlphaMode::Blend.to_uniform_parts(), (2, 0.0));
    }

    #[test]
    fn mask_cutoff_is_clamped_into_range() {
        assert_eq!(AlphaMode::Mask { cutoff: 5.0 }.to_uniform_parts().1, 1.0);
        assert_eq!(AlphaMode::Mask { cutoff: -3.0 }.to_uniform_parts().1, 0.0);
    }

    #[test]
    fn only_blend_counts_as_transparent() {
        assert!(AlphaMode::Blend.is_transparent());
        assert!(
            !AlphaMode::Mask { cutoff: 0.5 }.is_transparent(),
            "cutout stays in the opaque pass — that is the whole point of it",
        );
        assert!(!AlphaMode::Opaque.is_transparent());
    }

    #[test]
    fn material_fields_round_trip_into_the_uniform() {
        let material = Material {
            base_color_factor: [0.1, 0.2, 0.3, 0.4],
            metallic_factor: 0.5,
            roughness_factor: 0.6,
            emissive_factor: [1.5, 0.0, 0.25],
            normal_scale: 0.75,
            occlusion_strength: 0.8,
            alpha_mode: AlphaMode::Mask { cutoff: 0.3 },
        };
        let uniform = MaterialUniform::from(material);

        assert_eq!(uniform.base_color_factor, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(uniform.emissive_factor, [1.5, 0.0, 0.25]);
        assert_eq!(uniform.normal_scale, 0.75);
        assert_eq!(uniform.occlusion_strength, 0.8);
        assert_eq!(uniform.alpha_mode, 1);
        assert_eq!(uniform.alpha_cutoff, 0.3);
    }

    #[test]
    fn occlusion_strength_is_clamped_on_upload() {
        let material = Material {
            occlusion_strength: 4.0,
            ..Material::DEFAULT
        };
        assert_eq!(MaterialUniform::from(material).occlusion_strength, 1.0);
    }

    #[test]
    fn the_default_material_is_unchanged_by_the_new_fields() {
        // A material nobody has configured must render exactly as it did
        // before T-15: no emission, neutral normal scale, full occlusion
        // (which the neutral white map makes a no-op), opaque.
        let uniform = MaterialUniform::from(Material::DEFAULT);
        assert_eq!(uniform.emissive_factor, [0.0, 0.0, 0.0]);
        assert_eq!(uniform.alpha_mode, 0);
        assert_eq!(uniform.normal_scale, 1.0);
    }

    #[test]
    fn the_foliage_preset_is_a_cutout() {
        assert!(matches!(
            Material::FOLIAGE.alpha_mode,
            AlphaMode::Mask { .. }
        ));
        assert!(!Material::FOLIAGE.alpha_mode.is_transparent());
    }
}
