//! Procedural skybox math: the inverse view-projection matrix (to
//! reconstruct a view ray per pixel) plus a sun direction/color, and their
//! GPU-uniform representation.
//!
//! No cubemap/HDRI texture — there's no asset pipeline support for one yet
//! (Milestone 5's importers only handle 2D textures). The sky is instead
//! painted procedurally in `skybox.wgsl`: a horizon-to-zenith gradient
//! plus a glow toward the sun, keyed entirely off the reconstructed view
//! ray. Swapping in a real HDRI cubemap later is future work once the
//! asset pipeline supports one.

use glam::Vec3;

use crate::camera::Camera;

/// Packs `camera` and a directional light's direction/color into a
/// [`SkyboxUniform`] ready for GPU upload.
///
/// `sun_direction` follows [`crate::DirectionalLight::direction`]'s
/// convention (the direction the light travels *toward*, not toward the
/// source) — `skybox.wgsl` flips it internally, the same as `pbr.wgsl`'s
/// BRDF does.
pub fn skybox_uniform(camera: &Camera, sun_direction: Vec3, sun_color: Vec3) -> SkyboxUniform {
    let inverse_view_projection = camera.view_projection_matrix().inverse();
    SkyboxUniform {
        inverse_view_projection: inverse_view_projection.to_cols_array_2d(),
        camera_position: [camera.eye.x, camera.eye.y, camera.eye.z, 1.0],
        sun_direction: {
            let d = sun_direction.normalize_or_zero();
            [d.x, d.y, d.z, 0.0]
        },
        sun_color: [sun_color.x, sun_color.y, sun_color.z, 0.0],
    }
}

/// GPU-layout skybox inputs, ready to write into a uniform buffer.
///
/// Same `#[repr(C)]` + `Pod`/`Zeroable` pattern as [`crate::CameraUniform`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkyboxUniform {
    /// Column-major inverse view-projection matrix — unprojects a
    /// clip-space point on the far plane back into world space, to
    /// reconstruct a view ray per pixel.
    pub inverse_view_projection: [[f32; 4]; 4],
    /// World-space camera position (`xyz`; `w` unused, always `1.0`).
    pub camera_position: [f32; 4],
    /// Direction the sun travels *toward* (`xyz`; `w` unused) — see
    /// [`crate::DirectionalLight::direction`]'s convention.
    pub sun_direction: [f32; 4],
    /// Sun glow color (`xyz`; `w` unused).
    pub sun_color: [f32; 4],
}

#[cfg(test)]
mod tests {
    use glam::Mat4;

    use super::*;

    #[test]
    fn inverse_view_projection_undoes_the_cameras_own_projection() {
        let camera = Camera::new(Vec3::new(1.0, 2.0, 5.0), Vec3::ZERO, 16.0 / 9.0);
        let uniform = skybox_uniform(&camera, Vec3::NEG_Y, Vec3::ONE);

        let inverse = Mat4::from_cols_array_2d(&uniform.inverse_view_projection);
        let round_trip = inverse * camera.view_projection_matrix();

        // `round_trip` should be (approximately) the identity matrix.
        let identity = Mat4::IDENTITY.to_cols_array();
        for (actual, expected) in round_trip.to_cols_array().iter().zip(identity) {
            assert!((actual - expected).abs() < 1e-3, "{actual} != {expected}");
        }
    }

    #[test]
    fn camera_position_matches_eye() {
        let camera = Camera::new(Vec3::new(3.0, 4.0, 5.0), Vec3::ZERO, 1.0);
        let uniform = skybox_uniform(&camera, Vec3::NEG_Y, Vec3::ONE);
        assert_eq!(uniform.camera_position, [3.0, 4.0, 5.0, 1.0]);
    }

    #[test]
    fn sun_direction_is_normalized() {
        let camera = Camera::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let uniform = skybox_uniform(&camera, Vec3::new(0.0, -3.0, 0.0), Vec3::ONE);
        assert_eq!(uniform.sun_direction, [0.0, -1.0, 0.0, 0.0]);
    }

    #[test]
    fn zero_sun_direction_normalizes_to_zero_without_panicking() {
        let camera = Camera::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let uniform = skybox_uniform(&camera, Vec3::ZERO, Vec3::ONE);
        assert_eq!(uniform.sun_direction, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn uniform_bytes_are_112_bytes_matching_wgsl_layout() {
        let camera = Camera::new(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), 1.0);
        let uniform = skybox_uniform(&camera, Vec3::NEG_Y, Vec3::ONE);
        assert_eq!(bytemuck::bytes_of(&uniform).len(), 112);
    }
}
