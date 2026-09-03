//! Camera: view/projection matrices and their GPU-uniform representation.

use glam::{Mat4, Vec3};

use crate::bounds::Frustum;

/// A camera's projection: perspective (3D, with foreshortening) or
/// orthographic (2D/isometric, none — object size on screen is
/// independent of depth).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// Perspective projection: vertical field of view, in radians.
    Perspective {
        /// Vertical field of view, in radians.
        fov_y_radians: f32,
    },
    /// Orthographic projection: visible world-space height. Visible
    /// width is derived as `height * aspect_ratio`, so the frustum stays
    /// centered and undistorted regardless of viewport shape.
    Orthographic {
        /// Visible world-space height.
        height: f32,
    },
}

/// A camera: eye position, look-at target, and projection parameters.
///
/// Matrices follow wgpu's clip-space convention (right-handed, Y-up,
/// depth range `[0, 1]`) via `glam::camera::rh::proj::directx`, which
/// despite the name is the correct convention for wgpu/Metal/D3D — not
/// OpenGL's `[-1, 1]` depth range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// World-space camera position.
    pub eye: Vec3,
    /// World-space point the camera looks at.
    pub target: Vec3,
    /// World-space up direction (normalized).
    pub up: Vec3,
    /// Viewport width / height.
    pub aspect_ratio: f32,
    /// This camera's projection mode and its parameters.
    pub projection: Projection,
    /// Near clip plane distance (must be > 0).
    pub near: f32,
    /// Far clip plane distance (must be > `near`).
    pub far: f32,
}

impl Camera {
    /// A camera at `eye` looking at `target`, with standard `+Y` up, a
    /// 45-degree vertical FOV, and near/far planes of `0.1`/`100.0`.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_renderer::Camera;
    /// use glam::Vec3;
    ///
    /// let camera = Camera::new(Vec3::new(0.0, 1.0, 3.0), Vec3::ZERO, 16.0 / 9.0);
    /// assert_eq!(camera.up, Vec3::Y);
    /// ```
    pub fn new(eye: Vec3, target: Vec3, aspect_ratio: f32) -> Self {
        Self {
            eye,
            target,
            up: Vec3::Y,
            aspect_ratio,
            projection: Projection::Perspective {
                fov_y_radians: 45.0_f32.to_radians(),
            },
            near: 0.1,
            far: 100.0,
        }
    }

    /// An orthographic camera at `eye` looking at `target`, with standard
    /// `+Y` up and near/far planes of `0.1`/`100.0`. `height` is the
    /// visible world-space height; visible width is `height *
    /// aspect_ratio`.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_renderer::{Camera, Projection};
    /// use glam::Vec3;
    ///
    /// let camera =
    ///     Camera::new_orthographic(Vec3::new(0.0, 0.0, 3.0), Vec3::ZERO, 16.0 / 9.0, 10.0);
    /// assert_eq!(camera.projection, Projection::Orthographic { height: 10.0 });
    /// ```
    pub fn new_orthographic(eye: Vec3, target: Vec3, aspect_ratio: f32, height: f32) -> Self {
        Self {
            eye,
            target,
            up: Vec3::Y,
            aspect_ratio,
            projection: Projection::Orthographic { height },
            near: 0.1,
            far: 100.0,
        }
    }

    /// The view matrix: transforms world space into camera space.
    pub fn view_matrix(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    /// The projection matrix: transforms camera space into wgpu clip
    /// space (`[0, 1]` depth range).
    pub fn projection_matrix(&self) -> Mat4 {
        match self.projection {
            Projection::Perspective { fov_y_radians } => {
                glam::camera::rh::proj::directx::perspective(
                    fov_y_radians,
                    self.aspect_ratio,
                    self.near,
                    self.far,
                )
            }
            Projection::Orthographic { height } => {
                let half_height = height * 0.5;
                let half_width = half_height * self.aspect_ratio;
                glam::camera::rh::proj::directx::orthographic(
                    -half_width,
                    half_width,
                    -half_height,
                    half_height,
                    self.near,
                    self.far,
                )
            }
        }
    }

    /// The combined view-projection matrix (`projection * view`), the
    /// matrix shaders actually need to transform world-space vertices
    /// into clip space.
    pub fn view_projection_matrix(&self) -> Mat4 {
        self.projection_matrix() * self.view_matrix()
    }

    /// This camera's world-space view [`Frustum`], for culling geometry
    /// that lies outside what the camera can see.
    ///
    /// Derived from [`Camera::view_projection_matrix`], so it reflects the
    /// current `eye`/`target`/`projection`/`near`/`far` — rebuild it any
    /// frame the camera moves.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_renderer::{Aabb, Camera};
    /// use glam::Vec3;
    ///
    /// let camera = Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0);
    /// let frustum = camera.frustum();
    /// let at_target = Aabb { min: Vec3::splat(-0.5), max: Vec3::splat(0.5) };
    /// assert!(frustum.intersects_aabb(&at_target));
    /// ```
    pub fn frustum(&self) -> Frustum {
        Frustum::from_view_projection(&self.view_projection_matrix())
    }

    /// This camera's view-projection matrix and world-space position,
    /// packed for GPU upload.
    pub fn to_uniform(&self) -> CameraUniform {
        CameraUniform {
            view_projection: self.view_projection_matrix().to_cols_array_2d(),
            view_position: [self.eye.x, self.eye.y, self.eye.z, 1.0],
        }
    }
}

/// GPU-layout view-projection matrix and world-space eye position, ready
/// to write into a uniform buffer.
///
/// `view_position` is a `vec4` (not `vec3`) purely for WGSL uniform
/// alignment — `vec3<f32>` fields would otherwise need explicit padding
/// to stay 16-byte aligned; `w` is unused (`1.0`, the homogeneous-point
/// convention).
///
/// `#[repr(C)]` + [`bytemuck::Pod`]/[`bytemuck::Zeroable`] make this
/// safely castable to `&[u8]` via `bytemuck::bytes_of` for
/// `Queue::write_buffer`, with no risk of uninitialized padding bytes
/// (all fields are plain `f32`s) or platform-dependent layout (`repr(C)`
/// fixes it).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraUniform {
    /// Column-major view-projection matrix, matching WGSL's `mat4x4<f32>`
    /// memory layout.
    pub view_projection: [[f32; 4]; 4],
    /// World-space camera position (`xyz`; `w` unused, always `1.0`) —
    /// the BRDF's view vector is derived from this per-fragment.
    pub view_position: [f32; 4],
}

impl CameraUniform {
    /// The identity view-projection, camera at the origin. Mainly useful
    /// as a placeholder before a real [`Camera`] is available.
    pub const IDENTITY: Self = Self {
        view_projection: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
        view_position: [0.0, 0.0, 0.0, 1.0],
    };
}

impl Default for CameraUniform {
    /// [`CameraUniform::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_standard_up_and_default_projection_params() {
        let camera = Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0);
        assert_eq!(camera.up, Vec3::Y);
        assert_eq!(camera.near, 0.1);
        assert_eq!(camera.far, 100.0);
        assert_eq!(
            camera.projection,
            Projection::Perspective {
                fov_y_radians: 45.0_f32.to_radians()
            }
        );
    }

    #[test]
    fn new_orthographic_has_standard_up_default_clip_planes_and_given_height() {
        let camera = Camera::new_orthographic(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0, 10.0);
        assert_eq!(camera.up, Vec3::Y);
        assert_eq!(camera.near, 0.1);
        assert_eq!(camera.far, 100.0);
        assert_eq!(camera.projection, Projection::Orthographic { height: 10.0 });
    }

    #[test]
    fn view_matrix_moves_eye_to_origin() {
        let camera = Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0);
        let eye_in_view_space = camera.view_matrix().transform_point3(camera.eye);
        assert!(eye_in_view_space.length() < 1e-4);
    }

    #[test]
    fn projection_matrix_maps_near_plane_center_to_zero_depth() {
        let camera = Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0);
        // A point at (0, 0, -near) in view space sits exactly on the near
        // plane; wgpu's [0,1] depth convention maps that to clip-space z=0
        // after the perspective divide.
        let clip =
            camera
                .projection_matrix()
                .mul_vec4(glam::Vec4::new(0.0, 0.0, -camera.near, 1.0));
        assert!((clip.z / clip.w).abs() < 1e-4);
    }

    #[test]
    fn orthographic_projection_has_no_perspective_divide() {
        // Orthographic clip-space w is always 1: unlike perspective, depth
        // never scales x/y, i.e. no foreshortening.
        let camera = Camera::new_orthographic(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0, 10.0);
        let clip = camera
            .projection_matrix()
            .mul_vec4(glam::Vec4::new(1.0, 0.0, -1.0, 1.0));
        assert_eq!(clip.w, 1.0);
    }

    #[test]
    fn orthographic_projection_maps_half_height_to_clip_edge() {
        // A point at world-space y = height/2 in view space sits exactly
        // on the top edge of the frustum, which orthographic projection
        // maps to clip-space y = 1 regardless of depth.
        let camera = Camera::new_orthographic(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0, 10.0);
        let clip = camera
            .projection_matrix()
            .mul_vec4(glam::Vec4::new(0.0, 5.0, -1.0, 1.0));
        assert!((clip.y - 1.0).abs() < 1e-4);
    }

    #[test]
    fn orthographic_projection_scales_visible_width_by_aspect_ratio() {
        let camera = Camera::new_orthographic(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 2.0, 10.0);
        // height=10 aspect=2.0 -> visible width 20, half-width 10, so
        // world x=10 sits exactly on the right clip edge (x=1).
        let clip = camera
            .projection_matrix()
            .mul_vec4(glam::Vec4::new(10.0, 0.0, -1.0, 1.0));
        assert!((clip.x - 1.0).abs() < 1e-4);
    }

    #[test]
    fn to_uniform_matches_view_projection_matrix() {
        let camera = Camera::new(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO, 16.0 / 9.0);
        let expected = camera.view_projection_matrix().to_cols_array_2d();
        assert_eq!(camera.to_uniform().view_projection, expected);
    }

    #[test]
    fn to_uniform_carries_eye_position() {
        let camera = Camera::new(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO, 1.0);
        assert_eq!(camera.to_uniform().view_position, [1.0, 2.0, 3.0, 1.0]);
    }

    #[test]
    fn uniform_identity_matches_glam_identity() {
        assert_eq!(
            CameraUniform::IDENTITY.view_projection,
            Mat4::IDENTITY.to_cols_array_2d()
        );
    }

    #[test]
    fn uniform_default_matches_identity() {
        assert_eq!(CameraUniform::default(), CameraUniform::IDENTITY);
    }

    #[test]
    fn uniform_bytes_are_80_bytes_matching_wgsl_mat4x4_plus_vec4() {
        let uniform = CameraUniform::IDENTITY;
        assert_eq!(bytemuck::bytes_of(&uniform).len(), 80);
    }
}
