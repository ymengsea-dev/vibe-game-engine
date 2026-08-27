//! Shadow-map math: the light-space view-projection matrix used to render
//! a scene's depth from a directional light's point of view, and its
//! GPU-uniform representation.
//!
//! Only directional-light shadows exist right now — one shadow-casting sun
//! per scene, via a single orthographic projection. Point-light shadows
//! (cubemap or six-pass) are future work; see [`crate::ShadowMap`].

use glam::{Mat4, Vec3};

/// Width and height (in texels) of the shadow depth texture.
///
/// `1024` rather than a higher-fidelity `2048`/`4096` — plenty sharp for a
/// handful of objects in the example scene, and a smaller render target
/// keeps the shadow pass's GPU cost modest. Revisit once real scenes need
/// finer shadow detail.
pub const SHADOW_MAP_SIZE: u32 = 1024;

/// Builds the view-projection matrix for rendering a scene's depth from a
/// directional light's perspective, ready to both render the shadow map
/// with and sample it back in the main pass.
///
/// `direction` is the light's travel direction (same convention as
/// [`crate::DirectionalLight::direction`] — *toward* which the light
/// travels, not toward the source). The virtual shadow camera sits back
/// along `-direction` from `scene_center` by `2 * radius` and looks at
/// `scene_center` through an orthographic frustum wide enough to cover a
/// sphere of `radius` around it.
///
/// A zero-length `direction` (no meaningful light direction) falls back to
/// straight down (`-Y`) rather than producing a degenerate matrix — the
/// same "don't panic on bad input, degrade visibly instead" choice
/// [`crate::DirectionalLightUniform`]'s `normalize_or_zero` makes.
pub fn directional_light_view_projection(direction: Vec3, scene_center: Vec3, radius: f32) -> Mat4 {
    let direction = direction.normalize_or_zero();
    let direction = if direction == Vec3::ZERO {
        Vec3::NEG_Y
    } else {
        direction
    };

    let eye = scene_center - direction * radius * 2.0;
    // `look_at_mat4` requires an up vector not parallel to the view
    // direction; a light pointing (near-)straight up/down would make `+Y`
    // degenerate, so fall back to `+X` in that case.
    let up = if direction.abs_diff_eq(Vec3::Y, 1e-3) || direction.abs_diff_eq(Vec3::NEG_Y, 1e-3) {
        Vec3::X
    } else {
        Vec3::Y
    };

    let view = glam::camera::rh::view::look_at_mat4(eye, scene_center, up);
    let projection = glam::camera::rh::proj::directx::orthographic(
        -radius,
        radius,
        -radius,
        radius,
        0.01,
        radius * 4.0,
    );

    projection * view
}

/// GPU-layout light-space view-projection matrix, ready to write into a
/// uniform buffer.
///
/// Same `#[repr(C)]` + `Pod`/`Zeroable` pattern as [`crate::CameraUniform`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowUniform {
    /// Column-major light-space view-projection matrix, matching WGSL's
    /// `mat4x4<f32>` memory layout.
    pub light_space_matrix: [[f32; 4]; 4],
}

impl ShadowUniform {
    /// The identity light-space matrix. Mainly useful as a placeholder
    /// before a real shadow-casting light is available.
    pub const IDENTITY: Self = Self {
        light_space_matrix: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
}

impl Default for ShadowUniform {
    /// [`ShadowUniform::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl From<Mat4> for ShadowUniform {
    fn from(matrix: Mat4) -> Self {
        Self {
            light_space_matrix: matrix.to_cols_array_2d(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhead_light_projects_scene_center_to_screen_center() {
        let matrix = directional_light_view_projection(Vec3::NEG_Y, Vec3::ZERO, 5.0);
        let clip = matrix * glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() < 1e-4);
        assert!(ndc.z.abs() < 1.0); // wgpu depth range [0,1] — well within.
        assert!(ndc.z > 0.0);
    }

    #[test]
    fn degenerate_up_vector_case_does_not_panic_or_produce_nan() {
        // Light pointing straight down triggers the up-vector fallback.
        let down = directional_light_view_projection(Vec3::NEG_Y, Vec3::ZERO, 5.0);
        assert!(down.to_cols_array().iter().all(|c| c.is_finite()));

        // Light pointing straight up triggers it too.
        let up = directional_light_view_projection(Vec3::Y, Vec3::ZERO, 5.0);
        assert!(up.to_cols_array().iter().all(|c| c.is_finite()));
    }

    #[test]
    fn zero_direction_falls_back_to_straight_down_without_panicking() {
        let fallback = directional_light_view_projection(Vec3::ZERO, Vec3::ZERO, 5.0);
        let down = directional_light_view_projection(Vec3::NEG_Y, Vec3::ZERO, 5.0);
        assert_eq!(fallback, down);
    }

    #[test]
    fn different_directions_produce_different_matrices() {
        let a = directional_light_view_projection(Vec3::NEG_Y, Vec3::ZERO, 5.0);
        let b = directional_light_view_projection(Vec3::new(-0.4, -1.0, -0.3), Vec3::ZERO, 5.0);
        assert_ne!(a, b);
    }

    #[test]
    fn matrix_is_invertible() {
        let matrix =
            directional_light_view_projection(Vec3::new(-0.4, -1.0, -0.3), Vec3::ZERO, 5.0);
        assert!(matrix.determinant().abs() > 1e-8);
    }

    #[test]
    fn uniform_identity_matches_glam_identity() {
        assert_eq!(
            ShadowUniform::IDENTITY.light_space_matrix,
            Mat4::IDENTITY.to_cols_array_2d()
        );
    }

    #[test]
    fn uniform_default_matches_identity() {
        assert_eq!(ShadowUniform::default(), ShadowUniform::IDENTITY);
    }

    #[test]
    fn from_mat4_round_trips() {
        let matrix = directional_light_view_projection(Vec3::NEG_Y, Vec3::ZERO, 5.0);
        assert_eq!(
            ShadowUniform::from(matrix).light_space_matrix,
            matrix.to_cols_array_2d()
        );
    }

    #[test]
    fn uniform_bytes_are_64_bytes_matching_wgsl_mat4x4() {
        assert_eq!(bytemuck::bytes_of(&ShadowUniform::IDENTITY).len(), 64);
    }
}
