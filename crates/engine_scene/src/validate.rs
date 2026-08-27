//! Semantic validation for parsed scene data.
//!
//! Syntactically valid RON can still describe nonsense — NaN floats, a
//! rotation that isn't a unit quaternion, a camera whose near plane is
//! behind its far plane. This is the boundary that catches that, since a
//! [`crate::Scene`] may come from untrusted input (hand-edited, unknown
//! source, corrupted on disk).

use glam::{Quat, Vec3};

use crate::format::{
    AssetRef, CameraData, MeshRendererData, ProjectionData, SceneEntity, SpriteData, TransformData,
};

impl TransformData {
    /// Checks that every component is finite and that `rotation` is (to
    /// floating-point tolerance) a unit quaternion.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let translation = Vec3::from_array(self.translation);
        let rotation = Quat::from_array(self.rotation);
        let scale = Vec3::from_array(self.scale);

        if !translation.is_finite() {
            return Err(format!("translation is not finite: {translation:?}"));
        }
        if !scale.is_finite() {
            return Err(format!("scale is not finite: {scale:?}"));
        }
        if !rotation.is_finite() {
            return Err(format!("rotation is not finite: {rotation:?}"));
        }
        if !rotation.is_normalized() {
            return Err(format!(
                "rotation is not a unit quaternion (length {}): {rotation:?}",
                rotation.length()
            ));
        }
        Ok(())
    }
}

impl CameraData {
    /// Checks that every vector is finite and that the projection
    /// parameters describe a valid, non-degenerate frustum.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let eye = Vec3::from_array(self.eye);
        let target = Vec3::from_array(self.target);
        let up = Vec3::from_array(self.up);

        if !eye.is_finite() {
            return Err(format!("eye is not finite: {eye:?}"));
        }
        if !target.is_finite() {
            return Err(format!("target is not finite: {target:?}"));
        }
        if !up.is_finite() {
            return Err(format!("up is not finite: {up:?}"));
        }
        if !(self.near.is_finite() && self.near > 0.0) {
            return Err(format!("near must be finite and > 0, got {}", self.near));
        }
        if !(self.far.is_finite() && self.far > self.near) {
            return Err(format!(
                "far must be finite and > near ({}), got {}",
                self.near, self.far
            ));
        }
        if !(self.aspect_ratio.is_finite() && self.aspect_ratio > 0.0) {
            return Err(format!(
                "aspect_ratio must be finite and > 0, got {}",
                self.aspect_ratio
            ));
        }
        match self.projection {
            ProjectionData::Perspective { fov_y_radians } => {
                if !(fov_y_radians.is_finite()
                    && fov_y_radians > 0.0
                    && fov_y_radians < std::f32::consts::PI)
                {
                    return Err(format!(
                        "fov_y_radians must be finite and in (0, PI), got {fov_y_radians}"
                    ));
                }
            }
            ProjectionData::Orthographic { height } => {
                if !(height.is_finite() && height > 0.0) {
                    return Err(format!(
                        "orthographic height must be finite and > 0, got {height}"
                    ));
                }
            }
        }
        Ok(())
    }
}

impl AssetRef {
    /// Checks that this reference's `id` is valid UUID text.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.parse_id().map(|_| ())
    }
}

impl MeshRendererData {
    /// Checks that both asset references are valid.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.mesh
            .validate()
            .map_err(|reason| format!("mesh: {reason}"))?;
        self.material
            .validate()
            .map_err(|reason| format!("material: {reason}"))?;
        Ok(())
    }
}

impl SpriteData {
    /// Checks that `atlas` is a valid reference, `region` is non-empty,
    /// `size` is finite and positive, and `color` is finite.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.atlas
            .validate()
            .map_err(|reason| format!("atlas: {reason}"))?;
        if self.region.is_empty() {
            return Err("region must not be empty".to_string());
        }
        let [width, height] = self.size;
        if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
            return Err(format!(
                "size must be finite and positive, got {:?}",
                self.size
            ));
        }
        if !self.color.iter().all(|c| c.is_finite()) {
            return Err(format!("color must be finite, got {:?}", self.color));
        }
        Ok(())
    }
}

impl SceneEntity {
    /// Checks this entity's own components. Doesn't check `parent` — that
    /// needs the owning [`crate::Scene`]'s full entity list to validate
    /// against (bounds, acyclicity); see [`crate::Scene::validate`].
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(transform) = &self.transform {
            transform
                .validate()
                .map_err(|reason| format!("transform: {reason}"))?;
        }
        if let Some(camera) = &self.camera {
            camera
                .validate()
                .map_err(|reason| format!("camera: {reason}"))?;
        }
        if let Some(mesh_renderer) = &self.mesh_renderer {
            mesh_renderer
                .validate()
                .map_err(|reason| format!("mesh_renderer: {reason}"))?;
        }
        if let Some(sprite) = &self.sprite {
            sprite
                .validate()
                .map_err(|reason| format!("sprite: {reason}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_transform() -> TransformData {
        TransformData {
            translation: [1.0, 2.0, 3.0],
            rotation: Quat::IDENTITY.to_array(),
            scale: [1.0, 1.0, 1.0],
        }
    }

    fn valid_camera() -> CameraData {
        CameraData {
            eye: [0.0, 0.0, 5.0],
            target: [0.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            aspect_ratio: 16.0 / 9.0,
            projection: ProjectionData::Perspective {
                fov_y_radians: std::f32::consts::FRAC_PI_4,
            },
            near: 0.1,
            far: 100.0,
        }
    }

    fn valid_orthographic_camera() -> CameraData {
        CameraData {
            projection: ProjectionData::Orthographic { height: 10.0 },
            ..valid_camera()
        }
    }

    #[test]
    fn valid_transform_passes() {
        assert!(valid_transform().validate().is_ok());
    }

    #[test]
    fn transform_with_nan_translation_fails() {
        let mut t = valid_transform();
        t.translation = [f32::NAN, 0.0, 0.0];
        assert!(t.validate().is_err());
    }

    #[test]
    fn transform_with_non_unit_rotation_fails() {
        let mut t = valid_transform();
        t.rotation = [0.0, 0.0, 0.0, 2.0]; // length 2, not normalized
        assert!(t.validate().is_err());
    }

    #[test]
    fn transform_with_infinite_scale_fails() {
        let mut t = valid_transform();
        t.scale = [f32::INFINITY, 1.0, 1.0];
        assert!(t.validate().is_err());
    }

    #[test]
    fn valid_camera_passes() {
        assert!(valid_camera().validate().is_ok());
    }

    #[test]
    fn camera_with_zero_near_fails() {
        let mut c = valid_camera();
        c.near = 0.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_far_behind_near_fails() {
        let mut c = valid_camera();
        c.far = c.near;
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_zero_aspect_ratio_fails() {
        let mut c = valid_camera();
        c.aspect_ratio = 0.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_fov_at_pi_fails() {
        let mut c = valid_camera();
        c.projection = ProjectionData::Perspective {
            fov_y_radians: std::f32::consts::PI,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_negative_fov_fails() {
        let mut c = valid_camera();
        c.projection = ProjectionData::Perspective {
            fov_y_radians: -0.1,
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn valid_orthographic_camera_passes() {
        assert!(valid_orthographic_camera().validate().is_ok());
    }

    #[test]
    fn camera_with_zero_orthographic_height_fails() {
        let mut c = valid_orthographic_camera();
        c.projection = ProjectionData::Orthographic { height: 0.0 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_negative_orthographic_height_fails() {
        let mut c = valid_orthographic_camera();
        c.projection = ProjectionData::Orthographic { height: -1.0 };
        assert!(c.validate().is_err());
    }

    #[test]
    fn camera_with_nan_orthographic_height_fails() {
        let mut c = valid_orthographic_camera();
        c.projection = ProjectionData::Orthographic { height: f32::NAN };
        assert!(c.validate().is_err());
    }

    #[test]
    fn entity_with_both_valid_passes() {
        let entity = SceneEntity {
            transform: Some(valid_transform()),
            camera: Some(valid_camera()),
            ..Default::default()
        };
        assert!(entity.validate().is_ok());
    }

    #[test]
    fn entity_with_invalid_camera_fails_even_with_valid_transform() {
        let mut camera = valid_camera();
        camera.near = -1.0;
        let entity = SceneEntity {
            transform: Some(valid_transform()),
            camera: Some(camera),
            ..Default::default()
        };
        assert!(entity.validate().is_err());
    }

    #[test]
    fn empty_entity_passes() {
        assert!(SceneEntity::default().validate().is_ok());
    }

    #[test]
    fn entity_with_name_and_no_other_components_passes() {
        let entity = SceneEntity {
            name: Some("Player".to_string()),
            ..Default::default()
        };
        assert!(entity.validate().is_ok());
    }

    fn valid_asset_ref() -> AssetRef {
        AssetRef {
            id: uuid::Uuid::nil().to_string(),
        }
    }

    #[test]
    fn asset_ref_with_valid_uuid_passes() {
        assert!(valid_asset_ref().validate().is_ok());
    }

    #[test]
    fn asset_ref_with_malformed_id_fails() {
        let asset_ref = AssetRef {
            id: "not-a-uuid".to_string(),
        };
        assert!(asset_ref.validate().is_err());
    }

    fn valid_mesh_renderer() -> MeshRendererData {
        MeshRendererData {
            mesh: valid_asset_ref(),
            material: valid_asset_ref(),
        }
    }

    #[test]
    fn mesh_renderer_with_valid_refs_passes() {
        assert!(valid_mesh_renderer().validate().is_ok());
    }

    #[test]
    fn mesh_renderer_with_malformed_mesh_id_fails() {
        let mut mesh_renderer = valid_mesh_renderer();
        mesh_renderer.mesh.id = "garbage".to_string();
        assert!(mesh_renderer.validate().is_err());
    }

    #[test]
    fn mesh_renderer_with_malformed_material_id_fails() {
        let mut mesh_renderer = valid_mesh_renderer();
        mesh_renderer.material.id = "garbage".to_string();
        assert!(mesh_renderer.validate().is_err());
    }

    #[test]
    fn entity_with_valid_mesh_renderer_passes() {
        let entity = SceneEntity {
            mesh_renderer: Some(valid_mesh_renderer()),
            ..Default::default()
        };
        assert!(entity.validate().is_ok());
    }

    #[test]
    fn entity_with_invalid_mesh_renderer_fails() {
        let mut mesh_renderer = valid_mesh_renderer();
        mesh_renderer.mesh.id = "garbage".to_string();
        let entity = SceneEntity {
            mesh_renderer: Some(mesh_renderer),
            ..Default::default()
        };
        assert!(entity.validate().is_err());
    }

    fn valid_sprite() -> SpriteData {
        SpriteData {
            atlas: valid_asset_ref(),
            region: "0_0".to_string(),
            size: [1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }

    #[test]
    fn sprite_with_valid_fields_passes() {
        assert!(valid_sprite().validate().is_ok());
    }

    #[test]
    fn sprite_with_invalid_atlas_fails() {
        let mut sprite = valid_sprite();
        sprite.atlas.id = "garbage".to_string();
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn sprite_with_empty_region_fails() {
        let mut sprite = valid_sprite();
        sprite.region = String::new();
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn sprite_with_zero_size_fails() {
        let mut sprite = valid_sprite();
        sprite.size = [0.0, 1.0];
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn sprite_with_negative_size_fails() {
        let mut sprite = valid_sprite();
        sprite.size = [1.0, -1.0];
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn sprite_with_nan_size_fails() {
        let mut sprite = valid_sprite();
        sprite.size = [f32::NAN, 1.0];
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn sprite_with_infinite_color_fails() {
        let mut sprite = valid_sprite();
        sprite.color = [f32::INFINITY, 1.0, 1.0, 1.0];
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn entity_with_valid_sprite_passes() {
        let entity = SceneEntity {
            sprite: Some(valid_sprite()),
            ..Default::default()
        };
        assert!(entity.validate().is_ok());
    }

    #[test]
    fn entity_with_invalid_sprite_fails() {
        let mut sprite = valid_sprite();
        sprite.region = String::new();
        let entity = SceneEntity {
            sprite: Some(sprite),
            ..Default::default()
        };
        assert!(entity.validate().is_err());
    }
}
