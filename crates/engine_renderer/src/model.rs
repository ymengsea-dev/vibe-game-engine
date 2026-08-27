//! Per-object model matrix and its GPU-uniform representation.

use engine_utils::Transform;

/// GPU-layout model matrix, ready to write into a uniform buffer.
///
/// Same `#[repr(C)]` + `Pod`/`Zeroable` pattern as [`crate::CameraUniform`]
/// — see there for why that's safe.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelUniform {
    /// Column-major model matrix, matching WGSL's `mat4x4<f32>` memory
    /// layout.
    pub model: [[f32; 4]; 4],
}

impl ModelUniform {
    /// The identity model matrix (object-space == world-space).
    pub const IDENTITY: Self = Self {
        model: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
}

impl Default for ModelUniform {
    /// [`ModelUniform::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl From<&Transform> for ModelUniform {
    fn from(transform: &Transform) -> Self {
        Self {
            model: transform.to_matrix().to_cols_array_2d(),
        }
    }
}

impl From<Transform> for ModelUniform {
    fn from(transform: Transform) -> Self {
        Self::from(&transform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Vec3};

    #[test]
    fn identity_matches_glam_identity() {
        assert_eq!(
            ModelUniform::IDENTITY.model,
            Mat4::IDENTITY.to_cols_array_2d()
        );
    }

    #[test]
    fn default_matches_identity() {
        assert_eq!(ModelUniform::default(), ModelUniform::IDENTITY);
    }

    #[test]
    fn from_transform_matches_to_matrix() {
        let transform = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let uniform = ModelUniform::from(&transform);
        assert_eq!(uniform.model, transform.to_matrix().to_cols_array_2d());
    }

    #[test]
    fn from_owned_and_from_ref_agree() {
        let transform = Transform::from_translation(Vec3::new(4.0, 5.0, 6.0));
        assert_eq!(
            ModelUniform::from(transform),
            ModelUniform::from(&transform)
        );
    }
}
