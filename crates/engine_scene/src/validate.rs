//! Semantic validation for parsed scene data.
//!
//! Syntactically valid RON can still describe nonsense — NaN floats, a
//! rotation that isn't a unit quaternion, a camera whose near plane is
//! behind its far plane. This is the boundary that catches that, since a
//! [`crate::Scene`] may come from untrusted input (hand-edited, unknown
//! source, corrupted on disk).

use glam::{Quat, Vec3};
use std::path::{Component, Path};

use crate::format::{
    AssetRef, CameraData, ColliderData, ColliderShape, FoliageData, MeshRendererData,
    ProjectionData, RuleAction, RuleTrigger, SceneEntity, SpriteData, TilemapData, TransformData,
};

impl TilemapData {
    /// Rejects malformed sheet settings before the editor or scene loader uses them.
    pub fn validate(&self) -> Result<(), String> {
        self.atlas.validate()?;
        if self.columns == 0 || self.rows == 0 || self.columns > 4096 || self.rows > 4096 {
            return Err("tilemap sheet must have 1–4096 columns and rows".into());
        }
        if self.brush_column >= self.columns || self.brush_row >= self.rows {
            return Err("tilemap brush cell is outside the sheet grid".into());
        }
        if !self
            .cell_size
            .iter()
            .all(|value| value.is_finite() && (0.01..=10_000.0).contains(value))
        {
            return Err("tilemap cell size must be between 0.01 and 10,000".into());
        }
        Ok(())
    }
}

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
        if let Some(authored) = &self.material_override
            && (!authored
                .tint
                .iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
                || ![authored.metallic, authored.roughness]
                    .into_iter()
                    .flatten()
                    .all(|value| value.is_finite() && (0.0..=1.0).contains(&value)))
        {
            return Err("material override factors must be finite and in [0, 1]".into());
        }
        Ok(())
    }
}

impl FoliageData {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.mesh.validate()?;
        self.material.validate()?;
        if self.instances.len() > 20_000 {
            return Err("foliage exceeds 20,000 instances".into());
        }
        for (index, transform) in self.instances.iter().enumerate() {
            transform
                .validate()
                .map_err(|reason| format!("instance {index}: {reason}"))?;
        }
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
        if self.region.starts_with("grid:")
            && crate::sprite_atlas::GridRegion::parse(&self.region).is_none()
        {
            return Err(
                "grid region must be grid:columns,rows:column,row with a cell inside the grid"
                    .into(),
            );
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
        if let Some(animation) = self.animation {
            let grid = crate::sprite_atlas::GridRegion::parse(&self.region)
                .ok_or("animated sprites require a sprite-sheet cell")?;
            let start = u64::from(grid.row) * u64::from(grid.columns) + u64::from(grid.column);
            let available = u64::from(grid.columns) * u64::from(grid.rows) - start;
            if animation.frames == 0
                || animation.frames > 4096
                || u64::from(animation.frames) > available
            {
                return Err(
                    "animation frames must fit the remaining sheet cells (max 4096)".into(),
                );
            }
            if !animation.fps.is_finite() || !(0.1..=120.0).contains(&animation.fps) {
                return Err("animation fps must be between 0.1 and 120".into());
            }
        }
        Ok(())
    }
}

impl ColliderData {
    /// Checks that the shape's dimensions are finite and positive, and
    /// that a trimesh names a parseable asset.
    ///
    /// A zero or negative radius is not a harmless oddity: rapier will
    /// reject or misbehave on it, and the failure surfaces far from the
    /// file that caused it.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let positive = |name: &str, value: f32| -> Result<(), String> {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(format!("{name} must be finite and > 0, got {value}"))
            }
        };
        match &self.shape {
            ColliderShape::Ball { radius } => positive("radius", *radius),
            ColliderShape::Cuboid { half_extents } => {
                for (axis, value) in ["x", "y", "z"].iter().zip(half_extents) {
                    positive(&format!("half_extents.{axis}"), *value)?;
                }
                Ok(())
            }
            ColliderShape::Cylinder {
                half_height,
                radius,
            }
            | ColliderShape::Capsule {
                half_height,
                radius,
            } => {
                positive("half_height", *half_height)?;
                positive("radius", *radius)
            }
            ColliderShape::TriMesh { mesh } => mesh
                .parse_id()
                .map(|_| ())
                .map_err(|reason| format!("mesh: {reason}")),
        }
    }
}

impl SceneEntity {
    /// Checks this entity's own components. Doesn't check `parent` — that
    /// needs the owning [`crate::Scene`]'s full entity list to validate
    /// against (bounds, acyclicity); see [`crate::Scene::validate`].
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(id) = &self.object_id {
            uuid::Uuid::parse_str(id).map_err(|err| format!("object_id: {err}"))?;
        }
        if let Some(script) = &self.script {
            let path = Path::new(script);
            let normal = !script.is_empty()
                && !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)));
            let lua = path.starts_with("assets/scripts")
                && path.extension().and_then(|ext| ext.to_str()) == Some("lua");
            let rust = path.starts_with("src")
                && path.extension().and_then(|ext| ext.to_str()) == Some("rs");
            if !normal || !(lua || rust) {
                return Err(format!(
                    "script: invalid project-relative script path {script}"
                ));
            }
        }
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
        if let Some(foliage) = &self.foliage {
            foliage
                .validate()
                .map_err(|reason| format!("foliage: {reason}"))?;
        }
        if let Some(sprite) = &self.sprite {
            sprite
                .validate()
                .map_err(|reason| format!("sprite: {reason}"))?;
        }
        if let Some(tilemap) = &self.tilemap {
            tilemap
                .validate()
                .map_err(|reason| format!("tilemap: {reason}"))?;
        }
        if let Some(collider) = &self.collider {
            collider
                .validate()
                .map_err(|reason| format!("collider: {reason}"))?;
        }
        if let Some(emitter) = &self.audio_emitter {
            emitter
                .validate()
                .map_err(|reason| format!("audio_emitter: {reason}"))?;
        }
        if let Some(water) = self.water {
            engine_physics::WaterSurface::try_from(water)
                .map_err(|error| format!("water: {error}"))?;
        }
        if let Some(light) = self.point_light
            && (!light
                .color
                .iter()
                .all(|value| value.is_finite() && (0.0..=100.0).contains(value))
                || !light.intensity.is_finite()
                || !(0.0..=1000.0).contains(&light.intensity)
                || !light.range.is_finite()
                || !(0.0..=10_000.0).contains(&light.range)
                || light.range == 0.0)
        {
            return Err("point_light: color, intensity, or range is invalid".into());
        }
        if let Some(ui) = &self.ui {
            ui.validate().map_err(|reason| format!("ui: {reason}"))?;
        }
        if self.rules.len() > 16 {
            return Err("rules: an object may have at most 16 rules".into());
        }
        for (index, rule) in self.rules.iter().enumerate() {
            rule.validate(self.ui.as_ref())
                .map_err(|reason| format!("rule {index}: {reason}"))?;
        }
        Ok(())
    }
}

impl crate::RuleData {
    fn validate(&self, owner_ui: Option<&crate::UiElementData>) -> Result<(), String> {
        match &self.trigger {
            RuleTrigger::UiClicked
                if !owner_ui.is_some_and(|ui| ui.kind == crate::UiKind::Button) =>
            {
                return Err("click trigger requires an owning UI Button".into());
            }
            RuleTrigger::Named(name) => validate_event_name(name)?,
            _ => {}
        }
        if let Some(position) = self.graph_position
            && !position
                .iter()
                .all(|value| value.is_finite() && value.abs() <= 1_000_000.0)
        {
            return Err("graph position must be finite and within ±1,000,000".into());
        }
        if let Some(condition) = &self.condition {
            validate_variable(&condition.variable, condition.value)
                .map_err(|reason| format!("condition: {reason}"))?;
        }
        let target = match &self.action {
            RuleAction::SetUiText { target, text } => {
                if text.len() > 1024 {
                    return Err("UI text exceeds 1024 bytes".into());
                }
                target
            }
            RuleAction::PlayAudio { target } => target,
            RuleAction::EmitEvent { name } => return validate_event_name(name),
            RuleAction::SetVariable { name, value } => return validate_variable(name, *value),
            RuleAction::AddToVariable { name, amount } => return validate_variable(name, *amount),
        };
        uuid::Uuid::parse_str(target)
            .map_err(|_| "action target must be an object ID".to_string())?;
        Ok(())
    }
}

fn validate_event_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err("event name must be 1–64 printable bytes".into());
    }
    Ok(())
}

fn validate_variable(name: &str, value: f64) -> Result<(), String> {
    if !crate::rules::valid_variable_name(name) {
        return Err("variable name must be 1–64 printable bytes".into());
    }
    if !value.is_finite() {
        return Err("variable value must be finite".into());
    }
    Ok(())
}

impl crate::UiElementData {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.text.len() > 1024 {
            return Err("text exceeds 1024 bytes".into());
        }
        if !self.offset.iter().all(|value| value.is_finite()) {
            return Err("offset must be finite".into());
        }
        if !self
            .size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0 && *value <= 10000.0)
        {
            return Err("size must be finite and in (0, 10000]".into());
        }
        if !self
            .color
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            return Err("color must be in [0, 1]".into());
        }
        Ok(())
    }
}

impl crate::format::AudioEmitterData {
    /// Checks the emitter's numbers are usable: a non-finite gain would
    /// reach the mixer, and a negative radius draws an overlay inside
    /// out.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.gain.is_finite() || self.gain < 0.0 {
            return Err(format!("gain is not a usable level: {}", self.gain));
        }
        if !self.radius.is_finite() || self.radius < 0.0 {
            return Err(format!("radius is not a usable distance: {}", self.radius));
        }
        Ok(())
    }
}

impl crate::format::EnvironmentData {
    /// Checks the ambient sound count, references and gains.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.ambient_sounds.len() > Self::MAX_AMBIENT_SOUNDS {
            return Err(format!(
                "at most {} ambient sounds are allowed",
                Self::MAX_AMBIENT_SOUNDS
            ));
        }
        for (index, ambient) in self.ambient_sounds.iter().enumerate() {
            ambient
                .sound
                .validate()
                .map_err(|reason| format!("ambient sound {index}: {reason}"))?;
            if !ambient.gain.is_finite() || !(0.0..=4.0).contains(&ambient.gain) {
                return Err(format!("ambient sound {index}: gain must be in [0, 4]"));
            }
        }
        Ok(())
    }
}

impl crate::format::NavGridData {
    /// Checks the grid is a grid: non-empty, positive cells, a finite
    /// origin, and exactly one blocked flag per cell.
    ///
    /// A scene file is untrusted input, and a short `blocked` list would
    /// otherwise become an out-of-bounds read the first time something
    /// pathfinds.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!("empty grid: {}x{}", self.width, self.height));
        }
        if !self.cell_size.is_finite() || self.cell_size <= 0.0 {
            return Err(format!("cell size is not positive: {}", self.cell_size));
        }
        if !self.origin.iter().all(|value| value.is_finite()) {
            return Err(format!("origin is not finite: {:?}", self.origin));
        }
        if !self.is_well_formed() {
            return Err(format!(
                "{} blocked flags for {} cells",
                self.blocked.len(),
                self.cell_count()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_override_rejects_nonfinite_and_out_of_range_values() {
        let mut mesh = valid_mesh_renderer();
        mesh.material_override = Some(crate::MaterialOverrideData {
            tint: [f32::NAN, 1.0, 1.0],
            metallic: Some(0.5),
            roughness: None,
        });
        assert!(mesh.validate().is_err());
        mesh.material_override.as_mut().unwrap().tint = [1.0; 3];
        mesh.material_override.as_mut().unwrap().metallic = Some(1.2);
        assert!(mesh.validate().is_err());
    }

    #[test]
    fn foliage_rejects_invalid_instances_and_excessive_count() {
        let source = valid_mesh_renderer();
        let mut foliage = crate::FoliageData {
            mesh: source.mesh,
            material: source.material,
            instances: vec![crate::TransformData::from(
                engine_utils::Transform::IDENTITY,
            )],
        };
        assert!(foliage.validate().is_ok());
        foliage.instances[0].translation[0] = f32::NAN;
        assert!(foliage.validate().is_err());
        foliage.instances =
            vec![crate::TransformData::from(engine_utils::Transform::IDENTITY); 20_001];
        assert!(foliage.validate().is_err());
    }

    #[test]
    fn scene_rejects_escaping_or_wrong_language_script_paths() {
        for path in [
            "../outside.lua",
            "/tmp/outside.lua",
            "assets/scripts/main.rs",
            "src/main.lua",
        ] {
            let entity = SceneEntity {
                script: Some(path.to_owned()),
                ..Default::default()
            };
            assert!(entity.validate().is_err(), "accepted {path}");
        }
        let entity = SceneEntity {
            script: Some("assets/scripts/actor.lua".to_owned()),
            ..Default::default()
        };
        assert!(entity.validate().is_ok());
    }

    #[test]
    fn scene_rejects_invalid_object_id() {
        let entity = SceneEntity {
            object_id: Some("not-a-uuid".to_owned()),
            ..Default::default()
        };
        assert!(entity.validate().is_err());
    }

    #[test]
    fn scene_rejects_invalid_point_light_values() {
        let mut entity = SceneEntity {
            point_light: Some(crate::PointLightData::default()),
            ..Default::default()
        };
        entity.point_light.as_mut().unwrap().intensity = f32::NAN;
        assert!(entity.validate().is_err());
        entity.point_light.as_mut().unwrap().intensity = 1.0;
        entity.point_light.as_mut().unwrap().range = 0.0;
        assert!(entity.validate().is_err());
        entity.point_light.as_mut().unwrap().range = 5.0;
        entity.point_light.as_mut().unwrap().color[0] = -1.0;
        assert!(entity.validate().is_err());
    }

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
            material_override: None,
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
            z_order: 0.0,
            animation: None,
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
    fn sprite_rejects_out_of_grid_cell_and_keeps_legacy_region_names() {
        let mut sprite = valid_sprite();
        sprite.region = "grid:2,2:2,0".into();
        assert!(sprite.validate().is_err());
        sprite.region = "grid:0,2:0,0".into();
        assert!(sprite.validate().is_err());
        sprite.region = "coin_0".into();
        assert!(sprite.validate().is_ok());
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

    #[test]
    fn sprite_animation_rejects_invalid_range_and_speed() {
        let mut sprite = valid_sprite();
        sprite.region = "grid:2,2:1,1".into();
        sprite.animation = Some(crate::SpriteAnimationData {
            frames: 2,
            fps: 8.0,
            looping: true,
        });
        assert!(sprite.validate().is_err());
        sprite.region = "grid:2,2:0,0".into();
        sprite.animation.as_mut().unwrap().fps = f32::INFINITY;
        assert!(sprite.validate().is_err());
        sprite.animation.as_mut().unwrap().fps = 8.0;
        assert!(sprite.validate().is_ok());
        sprite.region = "full".into();
        assert!(sprite.validate().is_err());
    }

    #[test]
    fn entity_rejects_invalid_tilemap_settings() {
        let mut tilemap = TilemapData::new(AssetRef {
            id: uuid::Uuid::new_v4().to_string(),
        });
        tilemap.columns = 0;
        assert!(
            SceneEntity {
                tilemap: Some(tilemap.clone()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );

        tilemap.columns = 4;
        tilemap.brush_column = 4;
        assert!(tilemap.validate().is_err());
        tilemap.brush_column = 0;
        tilemap.cell_size[0] = f32::NAN;
        assert!(tilemap.validate().is_err());
        tilemap.cell_size[0] = 0.0;
        assert!(tilemap.validate().is_err());
    }

    #[test]
    fn event_rule_rejects_bad_owner_target_and_payload() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut entity = SceneEntity {
            rules: vec![crate::RuleData {
                trigger: RuleTrigger::UiClicked,
                action: RuleAction::SetUiText {
                    target: id,
                    text: "Ready".into(),
                },
                condition: None,
                graph_position: None,
            }],
            ..Default::default()
        };
        assert!(entity.validate().is_err());
        entity.ui = Some(crate::UiElementData::button());
        assert!(entity.validate().is_ok());
        entity.rules[0].trigger = RuleTrigger::Named("bad\nname".into());
        assert!(entity.validate().is_err());
        entity.rules[0].trigger = RuleTrigger::Named("ready".into());
        entity.rules[0].action = RuleAction::PlayAudio {
            target: "../escape".into(),
        };
        assert!(entity.validate().is_err());
    }

    #[test]
    fn event_rule_rejects_an_invalid_emitted_event_name() {
        let mut entity = SceneEntity {
            rules: vec![crate::RuleData {
                trigger: RuleTrigger::Named("start".into()),
                action: RuleAction::EmitEvent {
                    name: "bad\nname".into(),
                },
                condition: None,
                graph_position: None,
            }],
            ..Default::default()
        };
        assert!(entity.validate().is_err());
        entity.rules[0].action = RuleAction::EmitEvent {
            name: "okay".into(),
        };
        assert!(entity.validate().is_ok());
    }
}
