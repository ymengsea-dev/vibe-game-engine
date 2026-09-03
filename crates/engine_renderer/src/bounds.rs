//! Bounding volumes and view-frustum math for culling: [`Aabb`],
//! [`Plane`], [`Frustum`].
//!
//! Pure CPU math, no GPU resources — a [`Mesh`](crate::Mesh) carries its
//! object-space [`Aabb`] (computed once at upload), a caller derives a
//! [`Frustum`] from a camera's view-projection matrix each frame, and the
//! per-entity visibility test ([`Frustum::intersects_aabb`]) decides what
//! goes in the draw list. The test is deliberately *conservative*: it
//! reports "visible" for anything it cannot prove is fully outside, so it
//! never hides an object that should be on screen.

use glam::{Mat3, Mat4, Vec3, Vec4};

/// An axis-aligned bounding box, described by its minimum and maximum
/// corner. Used both as a mesh's object-space extent and, after
/// [`Aabb::transformed`], as its world-space extent for culling.
///
/// `min <= max` component-wise is an invariant every constructor here
/// upholds; a hand-built `Aabb` with `min > max` on some axis is treated
/// as empty on that axis by the intersection test rather than causing a
/// panic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// The corner with the smallest coordinate on every axis.
    pub min: Vec3,
    /// The corner with the largest coordinate on every axis.
    pub max: Vec3,
}

impl Aabb {
    /// The smallest [`Aabb`] containing every point in `points`, or `None`
    /// if the iterator is empty (an empty set has no bounding box).
    ///
    /// # Example
    ///
    /// ```
    /// use engine_renderer::Aabb;
    /// use glam::Vec3;
    ///
    /// let aabb = Aabb::from_points([Vec3::new(-1.0, 0.0, 2.0), Vec3::new(3.0, -4.0, 1.0)])
    ///     .expect("two points bound a box");
    /// assert_eq!(aabb.min, Vec3::new(-1.0, -4.0, 1.0));
    /// assert_eq!(aabb.max, Vec3::new(3.0, 0.0, 2.0));
    /// ```
    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Option<Self> {
        let mut iter = points.into_iter();
        let first = iter.next()?;
        let mut min = first;
        let mut max = first;
        for point in iter {
            min = min.min(point);
            max = max.max(point);
        }
        Some(Self { min, max })
    }

    /// The box's center point (`(min + max) / 2`).
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half the box's size on each axis (`(max - min) / 2`).
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// The smallest [`Aabb`] enclosing this box after it is transformed by
    /// `matrix` (an object-to-world model matrix, in general).
    ///
    /// Uses the center/half-extents form of Arvo's method: the transformed
    /// center is `matrix * center`, and the transformed half-extents are
    /// `abs(matrix_3x3) * half_extents` — the component-wise absolute value
    /// of the upper-left 3x3 applied to the extents. This yields the exact
    /// enclosing AABB under translation, rotation, and non-uniform scale in
    /// one matrix-vector product per part, without expanding all eight
    /// corners.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_renderer::Aabb;
    /// use glam::{Mat4, Vec3};
    ///
    /// let unit = Aabb { min: Vec3::splat(-0.5), max: Vec3::splat(0.5) };
    /// let moved = unit.transformed(&Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)));
    /// assert_eq!(moved.min, Vec3::new(9.5, -0.5, -0.5));
    /// assert_eq!(moved.max, Vec3::new(10.5, 0.5, 0.5));
    /// ```
    pub fn transformed(&self, matrix: &Mat4) -> Self {
        let center = self.center();
        let extents = self.half_extents();

        let linear = Mat3::from_mat4(*matrix);
        let abs_linear = Mat3::from_cols(
            linear.x_axis.abs(),
            linear.y_axis.abs(),
            linear.z_axis.abs(),
        );

        let new_center = matrix.transform_point3(center);
        let new_extents = abs_linear * extents;

        Self {
            min: new_center - new_extents,
            max: new_center + new_extents,
        }
    }
}

/// A plane in the form `normal · x + d = 0`, with `normal` unit-length and
/// pointing toward the *inside* of a [`Frustum`]. A point `p` is on the
/// inside (or on the surface) when `normal · p + d >= 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// Unit-length plane normal, pointing into the frustum.
    pub normal: Vec3,
    /// Signed distance from the origin to the plane along `normal`.
    pub d: f32,
}

impl Plane {
    /// Builds a plane from the coefficients `(a, b, c, d)` of
    /// `a·x + b·y + c·z + d = 0`, normalizing so `normal` is unit-length.
    ///
    /// A near-zero `(a, b, c)` (only possible from a degenerate
    /// view-projection matrix) yields a zero normal rather than a
    /// division by zero; the intersection test then treats that plane as
    /// non-constraining unless `d` itself is negative.
    fn from_coefficients(coefficients: Vec4) -> Self {
        let normal = coefficients.truncate();
        let length = normal.length();
        if length > f32::EPSILON {
            Self {
                normal: normal / length,
                d: coefficients.w / length,
            }
        } else {
            Self {
                normal: Vec3::ZERO,
                d: coefficients.w,
            }
        }
    }

    /// Signed distance from `point` to the plane: positive on the inside
    /// (the half-space `normal` points into), negative on the outside.
    pub fn signed_distance(&self, point: Vec3) -> f32 {
        self.normal.dot(point) + self.d
    }
}

/// The six planes — left, right, bottom, top, near, far — bounding a
/// camera's view volume, all pointing inward. Built from a view-projection
/// matrix with [`Frustum::from_view_projection`] and tested against world-
/// space [`Aabb`]s with [`Frustum::intersects_aabb`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    /// Left, right, bottom, top, near, far — in that order, each pointing
    /// into the volume.
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Extracts the six view-frustum planes from a combined view-projection
    /// matrix (`projection * view`), via the Gribb–Hartmann method.
    ///
    /// The near-plane row combination assumes wgpu's clip-space depth range
    /// of `[0, 1]` (`z >= 0` at the near plane), not OpenGL's `[-1, 1]`
    /// (`z >= -w`) — matching [`Camera::projection_matrix`](crate::Camera::projection_matrix).
    /// Works for both perspective and orthographic projections.
    pub fn from_view_projection(view_projection: &Mat4) -> Self {
        let r0 = view_projection.row(0);
        let r1 = view_projection.row(1);
        let r2 = view_projection.row(2);
        let r3 = view_projection.row(3);

        Self {
            planes: [
                Plane::from_coefficients(r3 + r0), // left:   x >= -w
                Plane::from_coefficients(r3 - r0), // right:  x <=  w
                Plane::from_coefficients(r3 + r1), // bottom: y >= -w
                Plane::from_coefficients(r3 - r1), // top:    y <=  w
                Plane::from_coefficients(r2),      // near:   z >=  0  (wgpu [0,1] depth)
                Plane::from_coefficients(r3 - r2), // far:    z <=  w
            ],
        }
    }

    /// Whether `aabb` is *not* provably outside the frustum — i.e. whether
    /// it should be drawn.
    ///
    /// Conservative: returns `false` only when `aabb` lies entirely on the
    /// outside of at least one plane (the positive-vertex test — check the
    /// box corner farthest along each inward normal). A box that straddles
    /// a plane, or sits in the region near a frustum edge that no single
    /// plane excludes, returns `true`. It never returns `false` for a box
    /// that is actually visible.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        for plane in &self.planes {
            // The corner of `aabb` farthest along this plane's inward
            // normal. If even that corner is outside, the whole box is.
            let positive_vertex = Vec3::select(plane.normal.cmpge(Vec3::ZERO), aabb.max, aabb.min);
            if plane.signed_distance(positive_vertex) < 0.0 {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Camera;
    use glam::Quat;

    #[test]
    fn from_points_is_none_for_an_empty_iterator() {
        assert_eq!(Aabb::from_points(std::iter::empty()), None);
    }

    #[test]
    fn from_points_of_a_single_point_has_equal_min_and_max() {
        let p = Vec3::new(1.0, 2.0, 3.0);
        let aabb = Aabb::from_points([p]).unwrap();
        assert_eq!(aabb.min, p);
        assert_eq!(aabb.max, p);
    }

    #[test]
    fn from_points_bounds_every_point() {
        let aabb = Aabb::from_points([
            Vec3::new(-2.0, 5.0, 0.0),
            Vec3::new(4.0, -1.0, 3.0),
            Vec3::new(0.0, 0.0, -7.0),
        ])
        .unwrap();
        assert_eq!(aabb.min, Vec3::new(-2.0, -1.0, -7.0));
        assert_eq!(aabb.max, Vec3::new(4.0, 5.0, 3.0));
    }

    #[test]
    fn center_and_half_extents_of_a_unit_box() {
        let aabb = Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        };
        assert_eq!(aabb.center(), Vec3::ZERO);
        assert_eq!(aabb.half_extents(), Vec3::splat(0.5));
    }

    #[test]
    fn transformed_by_translation_shifts_both_corners() {
        let aabb = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        };
        let out = aabb.transformed(&Mat4::from_translation(Vec3::new(5.0, -3.0, 2.0)));
        assert_eq!(out.min, Vec3::new(4.0, -4.0, 1.0));
        assert_eq!(out.max, Vec3::new(6.0, -2.0, 3.0));
    }

    #[test]
    fn transformed_by_uniform_scale_grows_extents() {
        let aabb = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        };
        let out = aabb.transformed(&Mat4::from_scale(Vec3::splat(3.0)));
        assert_eq!(out.min, Vec3::splat(-3.0));
        assert_eq!(out.max, Vec3::splat(3.0));
    }

    #[test]
    fn transformed_by_ninety_degree_rotation_swaps_extents() {
        // A box 2 wide on X, 4 tall on Y, rotated 90 deg about Z becomes
        // 4 wide on X, 2 tall on Y.
        let aabb = Aabb {
            min: Vec3::new(-1.0, -2.0, -0.5),
            max: Vec3::new(1.0, 2.0, 0.5),
        };
        let out = aabb.transformed(&Mat4::from_quat(Quat::from_rotation_z(
            std::f32::consts::FRAC_PI_2,
        )));
        assert!((out.half_extents() - Vec3::new(2.0, 1.0, 0.5)).length() < 1e-5);
    }

    #[test]
    fn transformed_by_forty_five_degree_rotation_grows_the_aabb() {
        let aabb = Aabb {
            min: Vec3::new(-1.0, -1.0, 0.0),
            max: Vec3::new(1.0, 1.0, 0.0),
        };
        let out = aabb.transformed(&Mat4::from_quat(Quat::from_rotation_z(
            std::f32::consts::FRAC_PI_4,
        )));
        let expected = 2.0_f32.sqrt();
        assert!((out.half_extents().x - expected).abs() < 1e-5);
        assert!((out.half_extents().y - expected).abs() < 1e-5);
    }

    #[test]
    fn extracted_planes_all_have_unit_normals() {
        let camera = Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 16.0 / 9.0);
        let frustum = Frustum::from_view_projection(&camera.view_projection_matrix());
        for plane in &frustum.planes {
            assert!((plane.normal.length() - 1.0).abs() < 1e-5);
        }
    }

    fn test_camera() -> Camera {
        Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0)
    }

    #[test]
    fn box_at_the_look_at_point_is_visible() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        let aabb = Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        };
        assert!(frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn box_behind_the_camera_is_culled() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        // Camera sits at z = 5 looking toward -z; z = 10 is behind it.
        let aabb = Aabb {
            min: Vec3::new(-0.5, -0.5, 9.5),
            max: Vec3::new(0.5, 0.5, 10.5),
        };
        assert!(!frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn box_far_below_the_screen_is_culled() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        let aabb = Aabb {
            min: Vec3::new(-0.5, -100.5, -0.5),
            max: Vec3::new(0.5, -99.5, 0.5),
        };
        assert!(!frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn box_beyond_the_far_plane_is_culled() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        // far = 100, camera at z = 5 looking toward -z: past far is z < -95.
        let aabb = Aabb {
            min: Vec3::new(-0.5, -0.5, -200.5),
            max: Vec3::new(0.5, 0.5, -199.5),
        };
        assert!(!frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn box_straddling_the_near_plane_is_visible() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        // near = 0.1, camera at z = 5: near plane is at z ~= 4.9.
        let aabb = Aabb {
            min: Vec3::new(-0.3, -0.3, 4.6),
            max: Vec3::new(0.3, 0.3, 5.2),
        };
        assert!(frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn huge_box_enclosing_the_whole_frustum_is_visible() {
        let frustum = Frustum::from_view_projection(&test_camera().view_projection_matrix());
        let aabb = Aabb {
            min: Vec3::splat(-1000.0),
            max: Vec3::splat(1000.0),
        };
        assert!(frustum.intersects_aabb(&aabb));
    }

    #[test]
    fn orthographic_frustum_culls_a_box_outside_its_visible_width() {
        let camera = Camera::new_orthographic(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0, 10.0);
        let frustum = Frustum::from_view_projection(&camera.view_projection_matrix());
        // Visible half-width is 5; a unit box centered at x = 20 is well
        // outside.
        let outside = Aabb {
            min: Vec3::new(19.5, -0.5, -0.5),
            max: Vec3::new(20.5, 0.5, 0.5),
        };
        let inside = Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        };
        assert!(!frustum.intersects_aabb(&outside));
        assert!(frustum.intersects_aabb(&inside));
    }

    #[test]
    fn camera_frustum_matches_manual_extraction() {
        let camera = Camera::new(Vec3::new(1.0, 2.0, 6.0), Vec3::ZERO, 4.0 / 3.0);
        let from_camera = camera.frustum();
        let manual = Frustum::from_view_projection(&camera.view_projection_matrix());
        for (a, b) in from_camera.planes.iter().zip(manual.planes.iter()) {
            assert!((a.normal - b.normal).length() < 1e-6);
            assert!((a.d - b.d).abs() < 1e-6);
        }
    }
}
