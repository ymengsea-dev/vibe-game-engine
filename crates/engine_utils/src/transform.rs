//! Engine-wide spatial transform type.

use glam::{Mat4, Quat, Vec3};

/// Position, rotation, and scale of an object in 3D space.
///
/// This is the one representation every subsystem that places things in
/// the world shares (renderer, ECS components in Milestone 3, scene
/// hierarchy in Milestone 4, physics bodies, ...), so it lives here in
/// `engine_utils` rather than owned by any single subsystem crate.
///
/// Follows glam/wgpu's right-handed convention: `+X` right, `+Y` up, `-Z`
/// forward (i.e. an unrotated transform "looks" down `-Z`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Position in world (or parent-local, once hierarchy exists) space.
    pub translation: Vec3,
    /// Orientation.
    pub rotation: Quat,
    /// Per-axis scale.
    pub scale: Vec3,
}

impl Transform {
    /// No translation, no rotation, unit scale.
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// A transform at `translation`, with identity rotation and unit scale.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_utils::Transform;
    /// use glam::Vec3;
    ///
    /// let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
    /// assert_eq!(t.translation, Vec3::new(1.0, 2.0, 3.0));
    /// ```
    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    /// A transform with `rotation`, at the origin with unit scale.
    pub fn from_rotation(rotation: Quat) -> Self {
        Self {
            rotation,
            ..Self::IDENTITY
        }
    }

    /// A transform with `scale`, at the origin with identity rotation.
    pub fn from_scale(scale: Vec3) -> Self {
        Self {
            scale,
            ..Self::IDENTITY
        }
    }

    /// Returns `self` with `translation` replaced. Builder-style.
    #[must_use]
    pub fn with_translation(mut self, translation: Vec3) -> Self {
        self.translation = translation;
        self
    }

    /// Returns `self` with `rotation` replaced. Builder-style.
    #[must_use]
    pub fn with_rotation(mut self, rotation: Quat) -> Self {
        self.rotation = rotation;
        self
    }

    /// Returns `self` with `scale` replaced. Builder-style.
    #[must_use]
    pub fn with_scale(mut self, scale: Vec3) -> Self {
        self.scale = scale;
        self
    }

    /// The 4x4 matrix this transform represents (scale, then rotate, then
    /// translate), for uploading to the GPU as a model matrix.
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Decomposes an affine matrix back into a TRS transform.
    ///
    /// Exact for any matrix produced by [`Transform::to_matrix`]. For a
    /// matrix carrying shear (e.g. the inverse of a transform with
    /// non-uniform scale, or a product of such), the scale/rotation are
    /// glam's best-fit decomposition — lossy, the same trade-off the
    /// Euler-angle rotation editor makes.
    pub fn from_matrix(matrix: Mat4) -> Self {
        let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
        Self {
            translation,
            rotation,
            scale,
        }
    }

    /// The transform that undoes `self`: `self.inverse().mul_transform(&self)`
    /// is the identity (up to floating-point error, and up to the
    /// decomposition caveat on [`Transform::from_matrix`] for
    /// non-uniform scale).
    pub fn inverse(&self) -> Self {
        Self::from_matrix(self.to_matrix().inverse())
    }

    /// The direction this transform faces: rotation applied to `-Z`.
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::NEG_Z
    }

    /// The direction to this transform's right: rotation applied to `+X`.
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }

    /// The direction above this transform: rotation applied to `+Y`.
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }

    /// Composes `self` as a parent transform with `child` as a
    /// parent-relative (local) transform, returning `child`'s effective
    /// transform in `self`'s space.
    ///
    /// This is the standard hierarchical TRS composition: applying
    /// `self`'s scale and rotation to `child`'s translation before adding
    /// `self`'s own translation, so a scaled/rotated parent correctly
    /// carries its children along with it.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_utils::Transform;
    /// use glam::Vec3;
    ///
    /// let parent = Transform::from_translation(Vec3::new(10.0, 0.0, 0.0));
    /// let child = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
    /// let world = parent.mul_transform(&child);
    /// assert_eq!(world.translation, Vec3::new(11.0, 0.0, 0.0));
    /// ```
    pub fn mul_transform(&self, child: &Transform) -> Transform {
        Transform {
            translation: self.translation + self.rotation * (self.scale * child.translation),
            rotation: self.rotation * child.rotation,
            scale: self.scale * child.scale,
        }
    }
}

impl Default for Transform {
    /// [`Transform::IDENTITY`].
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_has_no_translation_default_rotation_unit_scale() {
        let t = Transform::IDENTITY;
        assert_eq!(t.translation, Vec3::ZERO);
        assert_eq!(t.rotation, Quat::IDENTITY);
        assert_eq!(t.scale, Vec3::ONE);
    }

    #[test]
    fn default_matches_identity() {
        assert_eq!(Transform::default(), Transform::IDENTITY);
    }

    #[test]
    fn from_translation_keeps_identity_rotation_and_scale() {
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(t.rotation, Quat::IDENTITY);
        assert_eq!(t.scale, Vec3::ONE);
    }

    #[test]
    fn builder_methods_chain() {
        let t = Transform::IDENTITY
            .with_translation(Vec3::new(1.0, 0.0, 0.0))
            .with_scale(Vec3::splat(2.0));
        assert_eq!(t.translation, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(t.scale, Vec3::splat(2.0));
        assert_eq!(t.rotation, Quat::IDENTITY);
    }

    #[test]
    fn identity_matrix_is_glam_identity() {
        assert_eq!(Transform::IDENTITY.to_matrix(), Mat4::IDENTITY);
    }

    #[test]
    fn translation_matrix_moves_origin() {
        let t = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let transformed_origin = t.to_matrix().transform_point3(Vec3::ZERO);
        assert_eq!(transformed_origin, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn identity_faces_negative_z_with_standard_up_and_right() {
        let t = Transform::IDENTITY;
        assert_eq!(t.forward(), Vec3::NEG_Z);
        assert_eq!(t.right(), Vec3::X);
        assert_eq!(t.up(), Vec3::Y);
    }

    #[test]
    fn yaw_90_degrees_turns_forward_from_neg_z_toward_neg_x() {
        // Right-handed: +90 deg around +Y rotates -Z toward -X.
        let t = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let forward = t.forward();
        assert!((forward - Vec3::NEG_X).length() < 1e-5);
    }

    #[test]
    fn mul_identity_parent_returns_child_unchanged() {
        let child = Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(Transform::IDENTITY.mul_transform(&child), child);
    }

    #[test]
    fn mul_translates_child_by_parent_translation() {
        let parent = Transform::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let child = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(
            parent.mul_transform(&child).translation,
            Vec3::new(11.0, 0.0, 0.0)
        );
    }

    #[test]
    fn mul_applies_parent_scale_to_child_translation() {
        let parent = Transform::from_scale(Vec3::splat(2.0));
        let child = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(
            parent.mul_transform(&child).translation,
            Vec3::new(2.0, 0.0, 0.0)
        );
        assert_eq!(parent.mul_transform(&child).scale, Vec3::splat(2.0));
    }

    #[test]
    fn mul_applies_parent_rotation_to_child_translation() {
        // Parent rotated 90 deg around +Y: a child offset along +X ends
        // up offset along -Z (right-handed rotation).
        let parent = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let child = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
        let world = parent.mul_transform(&child);
        assert!((world.translation - Vec3::NEG_Z).length() < 1e-5);
    }

    #[test]
    fn mul_composes_rotations() {
        let parent = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4));
        let child = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_4));
        let world = parent.mul_transform(&child);
        let expected = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!((world.rotation.dot(expected)).abs() > 1.0 - 1e-5);
    }

    #[test]
    fn mul_multiplies_scales() {
        let parent = Transform::from_scale(Vec3::splat(2.0));
        let child = Transform::from_scale(Vec3::splat(3.0));
        assert_eq!(parent.mul_transform(&child).scale, Vec3::splat(6.0));
    }

    fn approx_eq(a: &Transform, b: &Transform) -> bool {
        (a.translation - b.translation).length() < 1e-4
            && a.rotation.dot(b.rotation).abs() > 1.0 - 1e-4
            && (a.scale - b.scale).length() < 1e-4
    }

    #[test]
    fn from_matrix_then_to_matrix_round_trips_a_trs() {
        let t = Transform {
            translation: Vec3::new(1.0, -2.0, 3.5),
            rotation: Quat::from_rotation_y(0.6) * Quat::from_rotation_x(-0.3),
            scale: Vec3::new(2.0, 2.0, 2.0),
        };
        assert!(approx_eq(&Transform::from_matrix(t.to_matrix()), &t));
    }

    #[test]
    fn inverse_composes_to_identity() {
        let t = Transform {
            translation: Vec3::new(4.0, 0.0, -1.0),
            rotation: Quat::from_rotation_z(1.1),
            scale: Vec3::splat(3.0),
        };
        assert!(approx_eq(
            &t.inverse().mul_transform(&t),
            &Transform::IDENTITY
        ));
        assert!(approx_eq(
            &t.mul_transform(&t.inverse()),
            &Transform::IDENTITY
        ));
    }
}
