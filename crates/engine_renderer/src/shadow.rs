//! Shadow-map math: the light-space view-projection matrices used to render
//! a scene's depth from each shadow-casting light, and their GPU-uniform
//! representation.
//!
//! The sun (the first directional light) casts *cascaded* shadows: the view
//! from the camera out to the shadow distance is cut into up to
//! [`MAX_CASCADES`] slices, each with its own orthographic map, so shadows
//! are sharp near the camera and still reach far. Up to
//! [`crate::MAX_SHADOWED_LIGHTS`] point or spot lights cast shadows too: a
//! spot light through one perspective map, a point light through six (one
//! per axis direction). Every map is a layer of a depth texture array in
//! [`crate::ShadowMap`]; [`ShadowFrame::build`] works out this frame's
//! matrices.

use glam::{Mat4, Vec3, Vec4};

use crate::{Camera, LightSet, MAX_SHADOWED_LIGHTS, Projection, ShadowedLight};

/// Most sun shadow cascades. Must match `MAX_CASCADES` in
/// `shaders/pbr_shared.wgsl`.
pub const MAX_CASCADES: usize = 4;
/// Shadow-map layers per shadowed point or spot light: a point light needs
/// one per axis direction; a spot light uses the first.
pub const LAYERS_PER_LIGHT: usize = 6;
/// Layers of the point/spot shadow texture array.
pub const LOCAL_SHADOW_LAYERS: usize = MAX_SHADOWED_LIGHTS * LAYERS_PER_LIGHT;
/// Every shadow view a frame can render: the cascades, then the local layers.
pub const MAX_SHADOW_VIEWS: usize = MAX_CASCADES + LOCAL_SHADOW_LAYERS;
/// How strongly cascade splits bunch up near the camera: 0 is even slices,
/// 1 is logarithmic. In between keeps near shadows sharp without starving
/// the far ones.
const CASCADE_SPLIT_LAMBDA: f32 = 0.7;
/// Nearest a point or spot light's shadow map sees, in world units.
const LOCAL_SHADOW_NEAR: f32 = 0.05;

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

/// Where each sun cascade ends, measured from the camera: `count` slices
/// between `near` and `far`, mixing even and logarithmic spacing. Unused
/// entries repeat `far`.
pub fn cascade_splits(near: f32, far: f32, count: usize) -> [f32; MAX_CASCADES] {
    let count = count.clamp(1, MAX_CASCADES);
    let near = near.max(0.01);
    let far = far.max(near + 0.01);
    let mut splits = [far; MAX_CASCADES];
    for (index, split) in splits.iter_mut().enumerate().take(count) {
        let share = (index + 1) as f32 / count as f32;
        let logarithmic = near * (far / near).powf(share);
        let even = near + (far - near) * share;
        *split = CASCADE_SPLIT_LAMBDA * logarithmic + (1.0 - CASCADE_SPLIT_LAMBDA) * even;
    }
    splits[count - 1] = far;
    splits
}

/// The orthographic sun matrix covering the part of `camera`'s view between
/// `from` and `to` (distances along its view direction).
///
/// The slice is wrapped in a sphere, so the map keeps its size as the camera
/// turns, and its centre is snapped to whole shadow texels, so shadow edges
/// don't shimmer as the camera moves. `size` is the map's width in texels.
pub fn cascade_matrix(camera: &Camera, from: f32, to: f32, sun_direction: Vec3, size: u32) -> Mat4 {
    let corners = slice_corners(camera, from, to);
    let centre = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
    let radius = corners
        .iter()
        .map(|corner| corner.distance(centre))
        .fold(0.0f32, f32::max)
        .max(0.5);
    // Rounded up a little, so tiny changes don't resize the map every frame.
    let radius = (radius * 16.0).ceil() / 16.0;

    let direction = light_direction(sun_direction);
    let view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, direction, up_for(direction));
    let light_centre = view.transform_point3(centre);
    let texel = 2.0 * radius / size.max(1) as f32;
    let snap = |value: f32| (value / texel).floor() * texel;
    let (x, y) = (snap(light_centre.x), snap(light_centre.y));
    // Looking down -Z: depth is -z. Reach well behind the slice so casters
    // between it and the sun still land in the map.
    let depth = -light_centre.z;
    let projection = glam::camera::rh::proj::directx::orthographic(
        x - radius,
        x + radius,
        y - radius,
        y + radius,
        depth - radius * 3.0,
        depth + radius,
    );
    projection * view
}

/// The eight corners of `camera`'s view between distances `from` and `to`.
fn slice_corners(camera: &Camera, from: f32, to: f32) -> [Vec3; 8] {
    let forward = (camera.target - camera.eye).normalize_or(Vec3::NEG_Z);
    let right = forward.cross(camera.up).normalize_or(Vec3::X);
    let up = right.cross(forward);
    let aspect = camera.aspect_ratio.max(0.01);
    let half = |distance: f32| match camera.projection {
        Projection::Perspective { fov_y_radians } => {
            let tan = (fov_y_radians * 0.5).tan();
            (tan * distance * aspect, tan * distance)
        }
        Projection::Orthographic { height } => (height * 0.5 * aspect, height * 0.5),
    };
    let mut corners = [Vec3::ZERO; 8];
    for (index, distance) in [from, to].into_iter().enumerate() {
        let (width, height) = half(distance);
        let centre = camera.eye + forward * distance;
        for (corner, (sx, sy)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
            .into_iter()
            .enumerate()
        {
            corners[index * 4 + corner] = centre + right * (sx * width) + up * (sy * height);
        }
    }
    corners
}

fn light_direction(direction: Vec3) -> Vec3 {
    let direction = direction.normalize_or_zero();
    if direction == Vec3::ZERO {
        Vec3::NEG_Y
    } else {
        direction
    }
}

/// An up vector that isn't parallel to `direction`.
fn up_for(direction: Vec3) -> Vec3 {
    if direction.y.abs() > 0.999 {
        Vec3::X
    } else {
        Vec3::Y
    }
}

/// A spot light's shadow matrix: a perspective as wide as its outer cone,
/// reaching `range`.
pub fn spot_light_matrix(position: Vec3, direction: Vec3, outer_angle: f32, range: f32) -> Mat4 {
    let direction = light_direction(direction);
    let view =
        glam::camera::rh::view::look_at_mat4(position, position + direction, up_for(direction));
    let fov = (outer_angle * 2.0).clamp(0.1, 170f32.to_radians());
    let projection = glam::camera::rh::proj::directx::perspective(
        fov,
        1.0,
        LOCAL_SHADOW_NEAR,
        range.max(LOCAL_SHADOW_NEAR * 2.0),
    );
    projection * view
}

/// The directions of a point light's six shadow faces, in layer order. The
/// shader picks a face the same way: by the largest axis of the direction
/// from the light, positive before negative.
pub const POINT_SHADOW_FACES: [Vec3; LAYERS_PER_LIGHT] = [
    Vec3::X,
    Vec3::NEG_X,
    Vec3::Y,
    Vec3::NEG_Y,
    Vec3::Z,
    Vec3::NEG_Z,
];

/// A point light's six shadow matrices, one per [`POINT_SHADOW_FACES`]
/// direction, each a 90° perspective reaching `range`.
pub fn point_light_matrices(position: Vec3, range: f32) -> [Mat4; LAYERS_PER_LIGHT] {
    POINT_SHADOW_FACES.map(|face| {
        let up = if face.y.abs() > 0.5 {
            Vec3::NEG_Z
        } else {
            Vec3::NEG_Y
        };
        let view = glam::camera::rh::view::look_at_mat4(position, position + face, up);
        let projection = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            LOCAL_SHADOW_NEAR,
            range.max(LOCAL_SHADOW_NEAR * 2.0),
        );
        projection * view
    })
}

/// One frame's shadow views: what [`crate::GpuContext::update_shadows`]
/// uploads and the depth passes render.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShadowFrame {
    /// The sun's cascade matrices, nearest first; empty when no directional
    /// light shines.
    pub cascades: Vec<Mat4>,
    /// Where each cascade ends, from the camera (see [`cascade_splits`]).
    pub cascade_splits: [f32; MAX_CASCADES],
    /// Point and spot light views: `(layer, matrix)`, layer `slot * 6 + face`.
    pub local: Vec<(usize, Mat4)>,
}

impl ShadowFrame {
    /// This frame's shadow views for `camera` and `lights`: `cascades` sun
    /// slices out to `lights.shadow_distance`, each `cascade_size` texels
    /// wide, and the lights [`LightSet::shadowed_lights`] picks for the
    /// camera's eye (so the slots match [`LightSet::to_uniform_for_view`]).
    pub fn build(camera: &Camera, lights: &LightSet, cascades: u32, cascade_size: u32) -> Self {
        let mut frame = Self::default();
        if let Some(sun) = lights.directional.first() {
            let count = (cascades as usize).clamp(1, MAX_CASCADES);
            let near = camera.near.max(0.01);
            let far = lights.shadow_distance.max(near + 1.0);
            frame.cascade_splits = cascade_splits(near, far, count);
            let mut from = near;
            for &to in frame.cascade_splits.iter().take(count) {
                frame.cascades.push(cascade_matrix(
                    camera,
                    from,
                    to,
                    sun.direction,
                    cascade_size,
                ));
                from = to;
            }
        }
        for (slot, light) in lights.shadowed_lights(camera.eye).into_iter().enumerate() {
            let first = slot * LAYERS_PER_LIGHT;
            match light {
                ShadowedLight::Spot(index) => {
                    if let Some(spot) = lights.spot.get(index) {
                        frame.local.push((
                            first,
                            spot_light_matrix(
                                spot.position,
                                spot.direction,
                                spot.outer_angle,
                                spot.range,
                            ),
                        ));
                    }
                }
                ShadowedLight::Point(index) => {
                    if let Some(point) = lights.point.get(index) {
                        for (face, matrix) in point_light_matrices(point.position, point.range)
                            .into_iter()
                            .enumerate()
                        {
                            frame.local.push((first + face, matrix));
                        }
                    }
                }
            }
        }
        frame
    }

    /// The scene-pass uniform for this frame.
    pub fn uniform(&self) -> ShadowsUniform {
        let mut uniform = ShadowsUniform::default();
        for (slot, matrix) in uniform.cascade_matrices.iter_mut().zip(&self.cascades) {
            *slot = matrix.to_cols_array_2d();
        }
        for &(layer, matrix) in &self.local {
            if let Some(slot) = uniform.local_matrices.get_mut(layer) {
                *slot = matrix.to_cols_array_2d();
            }
        }
        uniform.cascade_splits = self.cascade_splits;
        uniform.cascade_count = self.cascades.len().min(MAX_CASCADES) as u32;
        uniform
    }
}

/// Every shadow matrix the scene pass samples with, plus where the sun
/// cascades end. Mirrors `ShadowsUniform` in `pbr_shared.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowsUniform {
    /// Sun cascade matrices, nearest first.
    pub cascade_matrices: [[[f32; 4]; 4]; MAX_CASCADES],
    /// Point and spot light matrices, by layer.
    pub local_matrices: [[[f32; 4]; 4]; LOCAL_SHADOW_LAYERS],
    /// Where each cascade ends, from the camera.
    pub cascade_splits: [f32; MAX_CASCADES],
    /// How many cascades are in use; 0 means the sun casts no shadow.
    pub cascade_count: u32,
    /// Padding only.
    pub _padding: [u32; 3],
}

impl Default for ShadowsUniform {
    fn default() -> Self {
        let identity = Mat4::IDENTITY.to_cols_array_2d();
        Self {
            cascade_matrices: [identity; MAX_CASCADES],
            local_matrices: [identity; LOCAL_SHADOW_LAYERS],
            cascade_splits: [0.0; MAX_CASCADES],
            cascade_count: 0,
            _padding: [0; 3],
        }
    }
}

/// Where `point` lands in a shadow matrix's clip space, divided through.
pub fn shadow_ndc(matrix: Mat4, point: Vec3) -> Vec3 {
    let clip = matrix * Vec4::new(point.x, point.y, point.z, 1.0);
    clip.truncate() / clip.w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera::new(
            Vec3::new(0.0, 2.0, 10.0),
            Vec3::new(0.0, 2.0, 0.0),
            16.0 / 9.0,
        )
    }

    #[test]
    fn cascade_splits_grow_and_end_at_the_shadow_distance() {
        let splits = cascade_splits(0.1, 60.0, 3);
        assert!(splits[0] > 0.1 && splits[0] < splits[1] && splits[1] < splits[2]);
        assert_eq!(splits[2], 60.0);
        assert_eq!(splits[3], 60.0);
        // Near slices are thinner than an even split would make them.
        assert!(splits[0] < 20.0);
        assert_eq!(cascade_splits(0.1, 60.0, 1)[0], 60.0);
    }

    #[test]
    fn a_cascade_covers_its_slice_of_the_view() {
        let camera = camera();
        let matrix = cascade_matrix(&camera, 0.1, 8.0, Vec3::new(-0.4, -1.0, -0.3), 2048);
        for corner in slice_corners(&camera, 0.1, 8.0) {
            let ndc = shadow_ndc(matrix, corner);
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0, "{ndc:?}");
            assert!((0.0..=1.0).contains(&ndc.z), "{ndc:?}");
        }
    }

    #[test]
    fn a_cascade_moves_in_whole_texels() {
        let sun = Vec3::new(-0.4, -1.0, -0.3);
        let size = 1024;
        let a = cascade_matrix(&camera(), 0.1, 20.0, sun, size);
        let mut moved = camera();
        moved.eye.x += 0.0137;
        moved.target.x += 0.0137;
        let b = cascade_matrix(&moved, 0.1, 20.0, sun, size);
        // The world origin lands on the same texel grid in both maps.
        let texel = 2.0 / size as f32;
        let offset = (shadow_ndc(a, Vec3::ZERO) - shadow_ndc(b, Vec3::ZERO)).truncate() / texel;
        assert!((offset.x - offset.x.round()).abs() < 1e-2, "{offset:?}");
        assert!((offset.y - offset.y.round()).abs() < 1e-2, "{offset:?}");
    }

    #[test]
    fn a_point_lights_six_faces_see_every_direction() {
        let position = Vec3::new(1.0, 2.0, 3.0);
        let matrices = point_light_matrices(position, 10.0);
        for (face, direction) in POINT_SHADOW_FACES.iter().enumerate() {
            let ndc = shadow_ndc(matrices[face], position + *direction * 4.0);
            assert!(
                ndc.x.abs() < 1e-3 && ndc.y.abs() < 1e-3,
                "face {face}: {ndc:?}"
            );
            assert!((0.0..=1.0).contains(&ndc.z));
        }
    }

    #[test]
    fn a_spot_lights_map_sees_down_its_cone() {
        let matrix = spot_light_matrix(Vec3::ZERO, Vec3::NEG_Y, 30f32.to_radians(), 10.0);
        let ndc = shadow_ndc(matrix, Vec3::new(0.0, -5.0, 0.0));
        assert!(ndc.x.abs() < 1e-3 && ndc.y.abs() < 1e-3);
        assert!((0.0..=1.0).contains(&ndc.z));
        assert!(
            shadow_ndc(matrix, Vec3::new(0.0, -11.0, 0.0)).z > 1.0,
            "past its range"
        );
    }

    #[test]
    fn a_frame_has_a_view_per_cascade_and_light_face() {
        let mut lights = LightSet::new();
        lights.directional.push(crate::DirectionalLight {
            direction: Vec3::new(-0.4, -1.0, -0.3),
            color: Vec3::ONE,
            intensity: 3.0,
        });
        lights.point.push(crate::PointLight {
            position: Vec3::new(2.0, 2.0, 0.0),
            color: Vec3::ONE,
            intensity: 10.0,
            range: 8.0,
            casts_shadows: true,
        });
        lights.spot.push(crate::SpotLight {
            position: Vec3::new(0.0, 4.0, 0.0),
            direction: Vec3::NEG_Y,
            color: Vec3::ONE,
            intensity: 10.0,
            range: 8.0,
            inner_angle: 0.3,
            outer_angle: 0.5,
            casts_shadows: true,
        });
        let frame = ShadowFrame::build(&camera(), &lights, 3, 2048);
        assert_eq!(frame.cascades.len(), 3);
        assert_eq!(frame.local.len(), 6 + 1);
        let uniform = frame.uniform();
        assert_eq!(uniform.cascade_count, 3);
        assert_eq!(uniform.cascade_splits[2], lights.shadow_distance);
        assert_eq!(size_of::<ShadowsUniform>() % 16, 0);
        let shader = include_str!("shaders/pbr_shared.wgsl");
        assert!(shader.contains(&format!("const MAX_CASCADES: u32 = {MAX_CASCADES}u;")));
    }

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
