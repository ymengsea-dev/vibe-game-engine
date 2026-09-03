//! Serializable scene format: RON text, decoupled from the live ECS
//! component types.
//!
//! `engine_ecs`'s `Transform`/`Camera` components wrap `engine_utils`/
//! `engine_renderer` types directly; those aren't (and shouldn't be)
//! `Serialize`/`Deserialize` themselves — a scene *file* is a distinct
//! concern from a live `bevy_ecs` `World`, and coupling them would mean
//! every engine type's on-disk shape is dictated by its runtime shape.
//! [`TransformData`]/[`CameraData`] are the on-disk shapes; `From`/`Into`
//! bridge to the live types.
//!
//! [`MeshRenderer`](engine_ecs's mesh component) isn't fully representable
//! here yet — it owns live GPU handles, not describable data — but
//! [`MeshRendererData`] carries the part that *is* describable: which mesh
//! and material asset it should resolve to. `engine_scene` deliberately
//! doesn't depend on `engine_asset` for this (a much heavier crate: tokio,
//! gltf, image, notify, for what would only be a 16-byte identity type
//! here); [`AssetRef`] stores the same UUID identity space as
//! `engine_asset::AssetId` as plain text. Turning a `MeshRendererData`
//! into a live `MeshRenderer` needs Stage 3's asset-handle work — this is
//! just the reference.
//!
//! [`SpriteData`] is the same idea for `engine_ecs`'s `Sprite` component:
//! which atlas asset and named/gridded region it should resolve to, plus
//! the size/tint that *are* plain data already. Also unresolved here —
//! turning `region` into a real UV rect needs a live `AtlasLayout`, which
//! only whoever spawns the entity (not this crate) has.

use serde::{Deserialize, Serialize};

use crate::error::SceneError;

/// On-disk position/rotation/scale — the serializable form of
/// [`engine_utils::Transform`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransformData {
    /// Position.
    pub translation: [f32; 3],
    /// Orientation, as a quaternion in `[x, y, z, w]` order.
    pub rotation: [f32; 4],
    /// Per-axis scale.
    pub scale: [f32; 3],
}

impl From<engine_utils::Transform> for TransformData {
    fn from(transform: engine_utils::Transform) -> Self {
        Self {
            translation: transform.translation.to_array(),
            rotation: transform.rotation.to_array(),
            scale: transform.scale.to_array(),
        }
    }
}

impl From<TransformData> for engine_utils::Transform {
    fn from(data: TransformData) -> Self {
        Self {
            translation: glam::Vec3::from_array(data.translation),
            rotation: glam::Quat::from_array(data.rotation),
            scale: glam::Vec3::from_array(data.scale),
        }
    }
}

/// On-disk projection — the serializable form of
/// [`engine_renderer::Projection`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ProjectionData {
    /// Perspective projection: vertical field of view, in radians.
    Perspective {
        /// Vertical field of view, in radians.
        fov_y_radians: f32,
    },
    /// Orthographic projection: visible world-space height.
    Orthographic {
        /// Visible world-space height.
        height: f32,
    },
}

impl From<engine_renderer::Projection> for ProjectionData {
    fn from(projection: engine_renderer::Projection) -> Self {
        match projection {
            engine_renderer::Projection::Perspective { fov_y_radians } => {
                Self::Perspective { fov_y_radians }
            }
            engine_renderer::Projection::Orthographic { height } => Self::Orthographic { height },
        }
    }
}

impl From<ProjectionData> for engine_renderer::Projection {
    fn from(data: ProjectionData) -> Self {
        match data {
            ProjectionData::Perspective { fov_y_radians } => Self::Perspective { fov_y_radians },
            ProjectionData::Orthographic { height } => Self::Orthographic { height },
        }
    }
}

/// On-disk camera — the serializable form of [`engine_renderer::Camera`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraData {
    /// World-space camera position.
    pub eye: [f32; 3],
    /// World-space point the camera looks at.
    pub target: [f32; 3],
    /// World-space up direction.
    pub up: [f32; 3],
    /// Viewport width / height.
    pub aspect_ratio: f32,
    /// This camera's projection mode and its parameters.
    pub projection: ProjectionData,
    /// Near clip plane distance.
    pub near: f32,
    /// Far clip plane distance.
    pub far: f32,
}

impl From<engine_renderer::Camera> for CameraData {
    fn from(camera: engine_renderer::Camera) -> Self {
        Self {
            eye: camera.eye.to_array(),
            target: camera.target.to_array(),
            up: camera.up.to_array(),
            aspect_ratio: camera.aspect_ratio,
            projection: camera.projection.into(),
            near: camera.near,
            far: camera.far,
        }
    }
}

impl From<CameraData> for engine_renderer::Camera {
    fn from(data: CameraData) -> Self {
        Self {
            eye: glam::Vec3::from_array(data.eye),
            target: glam::Vec3::from_array(data.target),
            up: glam::Vec3::from_array(data.up),
            aspect_ratio: data.aspect_ratio,
            projection: data.projection.into(),
            near: data.near,
            far: data.far,
        }
    }
}

/// A reference to an asset by UUID — the same identity space as
/// `engine_asset::AssetId`, stored as that id's canonical text so scene
/// `.ron` files stay human-diffable. See the module docs for why this
/// doesn't hold an `engine_asset::AssetId` directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    /// Canonical UUID text (e.g. `"3fa0..."`), matching
    /// `engine_asset::AssetId`'s `Display` output.
    pub id: String,
}

impl AssetRef {
    /// Parses `id` as a UUID — the only way this reference can be
    /// malformed, since it carries no other data.
    ///
    /// Returns `uuid::Uuid` rather than `engine_asset::AssetId`: resolving
    /// a reference into a real asset id needs `AssetId::from_uuid`, one
    /// layer up, where `engine_asset` is already a dependency.
    ///
    /// # Errors
    ///
    /// Returns a description if `id` isn't valid UUID text.
    pub fn parse_id(&self) -> Result<uuid::Uuid, String> {
        self.id
            .parse()
            .map_err(|err| format!("invalid asset id {:?}: {err}", self.id))
    }
}

/// On-disk mesh/material — the serializable, reference-only form of what
/// will eventually resolve into a live `engine_ecs::MeshRenderer`. See the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshRendererData {
    /// The mesh asset this entity renders.
    pub mesh: AssetRef,
    /// The material asset this entity renders with.
    pub material: AssetRef,
}

/// On-disk sprite — the serializable, reference-only form of what will
/// resolve into a live `engine_ecs::Sprite`. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteData {
    /// The atlas texture asset this sprite samples.
    pub atlas: AssetRef,
    /// The named (or gridded, e.g. `"0_0"`) region within `atlas` this
    /// sprite samples.
    pub region: String,
    /// World-space width/height.
    pub size: [f32; 2],
    /// Linear RGBA tint.
    pub color: [f32; 4],
}

/// One entity's serializable components. Every field is optional — an
/// entity may carry any subset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneEntity {
    /// This entity's display name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index of this entity's parent within the owning [`Scene`]'s
    /// `entities`, if any — `None` means a root entity.
    ///
    /// Only meaningful inside a [`Scene`]: [`Scene::validate`] is what
    /// checks it's in bounds and acyclic (a lone [`SceneEntity`], e.g.
    /// wrapped in a [`crate::Prefab`], has no siblings to validate
    /// against, so `SceneEntity`'s own (crate-private) validation doesn't
    /// look at this field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// This entity's transform, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformData>,
    /// This entity's camera, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraData>,
    /// This entity's mesh/material, as asset references, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_renderer: Option<MeshRendererData>,
    /// A project-relative path to the asset file this entity was created
    /// from, if any — set by the editor's drag-an-asset-into-the-scene
    /// action, before a real import/GPU resolution exists. Additive and
    /// optional, so pre-existing scene files parse unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_source: Option<String>,
    /// The stable id (canonical UUID text, from the asset's `.meta`
    /// sidecar) of the file `asset_source` points at, if known. The
    /// editor re-resolves this to a current path on load, so the
    /// reference survives a rename or move of the source file. Additive
    /// and optional, like `asset_source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// This entity's 2D sprite, as an atlas reference, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<SpriteData>,
    /// Whether the editor has disabled this entity (hidden from the
    /// Scene view, greyed in the hierarchy). Additive and optional, so
    /// pre-existing scene files parse unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// Whether the editor has marked this entity static. Additive and
    /// optional, like `disabled`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_static: bool,
    /// Whether the editor has locked this entity (gizmo / batch move /
    /// reparent-drag skip it). Additive and optional, like `disabled`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
}

/// `#[serde(skip_serializing_if)]` predicate for a `bool` that defaults
/// to `false` — keeps the field out of the file unless it's `true`.
fn is_false(value: &bool) -> bool {
    !*value
}

/// A serializable scene: a flat list of entities, with parent/child
/// hierarchy expressed as index references (see [`SceneEntity::parent`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// This scene's on-disk format version. Absent from RON text parsed
    /// from a file written before this field existed — such a file is
    /// implicitly version `1`; see [`crate::CURRENT_SCENE_VERSION`] and
    /// [`Scene::from_ron_str`], which migrates to the current version on
    /// parse.
    #[serde(default = "legacy_scene_version")]
    pub version: u32,
    /// This scene's entities, in no particular order (order carries no
    /// meaning; use [`SceneEntity::parent`] for structure).
    #[serde(default)]
    pub entities: Vec<SceneEntity>,
}

/// Serde default for [`Scene::version`] when the field is missing from
/// parsed RON text: every scene file saved before versioning existed was
/// implicitly format `1`.
fn legacy_scene_version() -> u32 {
    1
}

impl Default for Scene {
    /// An empty scene at [`crate::CURRENT_SCENE_VERSION`] — not version
    /// `1`: a freshly code-constructed scene is always current-format,
    /// never a legacy one read from disk.
    fn default() -> Self {
        Self {
            version: crate::CURRENT_SCENE_VERSION,
            entities: Vec::new(),
        }
    }
}

impl Scene {
    /// Serializes this scene to pretty-printed RON text.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::Serialize`] on failure. In practice this
    /// should never happen for [`Scene`] (no floats that could be
    /// non-finite-and-therefore-unrepresentable in the fields RON cares
    /// about, no maps with non-string keys, ...), but the underlying `ron`
    /// call is fallible, so this stays a `Result` rather than assuming
    /// that never changes.
    pub fn to_ron_string(&self) -> Result<String, SceneError> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|err| SceneError::Serialize(err.to_string()))
    }

    /// Parses a scene from RON text, migrating it to
    /// [`crate::CURRENT_SCENE_VERSION`] if it was written by an older
    /// version of this engine.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::Deserialize`] if `text` isn't valid RON, or
    /// doesn't match [`Scene`]'s shape. Treat all scene text as untrusted
    /// input (it may come from a file on disk, or worse) — this rejects
    /// malformed input rather than panicking; see [`SceneError::Deserialize`]'s
    /// docs for what's still *not* validated here. Returns
    /// [`SceneError::UnsupportedVersion`] if the parsed scene's `version`
    /// is newer than this engine understands.
    pub fn from_ron_str(text: &str) -> Result<Self, SceneError> {
        let scene: Self =
            ron::from_str(text).map_err(|err| SceneError::Deserialize(err.to_string()))?;
        crate::migration::migrate(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};

    #[test]
    fn transform_data_round_trips_through_engine_utils_transform() {
        let original = engine_utils::Transform {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(0.5),
            scale: Vec3::new(2.0, 2.0, 2.0),
        };
        let data = TransformData::from(original);
        let round_tripped = engine_utils::Transform::from(data);
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn camera_data_round_trips_through_engine_renderer_camera() {
        let original = engine_renderer::Camera::new(Vec3::new(1.0, 2.0, 3.0), Vec3::ZERO, 1.5);
        let data = CameraData::from(original);
        let round_tripped = engine_renderer::Camera::from(data);
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn orthographic_camera_data_round_trips_through_engine_renderer_camera() {
        let original = engine_renderer::Camera::new_orthographic(
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::ZERO,
            1.5,
            10.0,
        );
        let data = CameraData::from(original);
        assert_eq!(
            data.projection,
            ProjectionData::Orthographic { height: 10.0 }
        );
        let round_tripped = engine_renderer::Camera::from(data);
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn empty_scene_round_trips_through_ron() {
        let scene = Scene::default();
        let ron_text = scene.to_ron_string().unwrap();
        let parsed = Scene::from_ron_str(&ron_text).unwrap();
        assert_eq!(parsed, scene);
    }

    #[test]
    fn scene_with_entities_round_trips_through_ron() {
        let scene = Scene {
            entities: vec![
                SceneEntity {
                    name: Some("Box".to_string()),
                    transform: Some(TransformData::from(
                        engine_utils::Transform::from_translation(Vec3::new(1.0, 0.0, 0.0)),
                    )),
                    ..Default::default()
                },
                SceneEntity {
                    parent: Some(0),
                    camera: Some(CameraData::from(engine_renderer::Camera::new(
                        Vec3::new(0.0, 1.0, 5.0),
                        Vec3::ZERO,
                        16.0 / 9.0,
                    ))),
                    mesh_renderer: Some(MeshRendererData {
                        mesh: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                        material: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                    }),
                    sprite: Some(SpriteData {
                        atlas: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                        region: "0_0".to_string(),
                        size: [1.0, 1.0],
                        color: [1.0, 1.0, 1.0, 1.0],
                    }),
                    ..Default::default()
                },
                SceneEntity::default(),
            ],
            ..Default::default()
        };

        let ron_text = scene.to_ron_string().unwrap();
        let parsed = Scene::from_ron_str(&ron_text).unwrap();
        assert_eq!(parsed, scene);
    }

    #[test]
    fn scene_version_defaults_to_current_and_round_trips() {
        let scene = Scene::default();
        assert_eq!(scene.version, crate::CURRENT_SCENE_VERSION);
        let ron_text = scene.to_ron_string().unwrap();
        assert!(ron_text.contains(&format!("version: {}", crate::CURRENT_SCENE_VERSION)));
    }

    #[test]
    fn asset_ref_parse_id_accepts_valid_uuid_text() {
        let asset_ref = AssetRef {
            id: uuid::Uuid::nil().to_string(),
        };
        assert_eq!(asset_ref.parse_id().unwrap(), uuid::Uuid::nil());
    }

    #[test]
    fn asset_ref_parse_id_rejects_malformed_text() {
        let asset_ref = AssetRef {
            id: "not-a-uuid".to_string(),
        };
        assert!(asset_ref.parse_id().is_err());
    }

    #[test]
    fn from_ron_str_rejects_malformed_ron() {
        let err = Scene::from_ron_str("not valid ron {{{").unwrap_err();
        assert!(matches!(err, SceneError::Deserialize(_)));
    }

    #[test]
    fn from_ron_str_rejects_wrong_shape() {
        // Valid RON, wrong shape for `Scene` (expects a struct with an
        // `entities` field, not a bare integer).
        let err = Scene::from_ron_str("42").unwrap_err();
        assert!(matches!(err, SceneError::Deserialize(_)));
    }

    #[test]
    fn missing_entities_field_defaults_to_empty() {
        // `entities` is `#[serde(default)]`, so a scene file that omits
        // it entirely (e.g. hand-written, or from an older format
        // version) still parses instead of erroring.
        let parsed = Scene::from_ron_str("()").unwrap();
        assert_eq!(parsed, Scene::default());
    }

    #[test]
    fn entity_with_only_camera_field_set_parses() {
        let parsed = Scene::from_ron_str("(entities: [(camera: None)])").unwrap();
        assert_eq!(parsed.entities.len(), 1);
        assert!(parsed.entities[0].transform.is_none());
        assert!(parsed.entities[0].camera.is_none());
    }
}
