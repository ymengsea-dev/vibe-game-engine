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
//! doesn't depend on `engine_asset` for this (a much heavier crate: gltf,
//! image, notify, for what would only be a 16-byte identity type
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
    /// `true` once the entity's `Transform` is the camera's pose.
    /// `false` (the default, for scenes written before this field existed)
    /// means `eye`/`target`/`up` are authoritative and are adopted into the
    /// transform on load. `eye`/`target`/`up` are still written on save,
    /// derived from the transform, so older SDKs open the same view.
    #[serde(default)]
    pub pose_from_transform: bool,
    /// Whether this is the scene's active camera.
    #[serde(default)]
    pub active: bool,
}

/// What input orbits a third-person follow camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OrbitInput {
    /// The camera never orbits.
    None,
    /// Mouse movement orbits.
    #[default]
    Mouse,
    /// The right thumbstick orbits.
    RightStick,
    /// Mouse and right thumbstick both orbit.
    Both,
}

/// How a follow camera tracks its target.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CameraFollowMode {
    /// Behind and above the target, orbiting around it.
    ThirdPerson {
        /// Distance from the look point to the camera.
        distance: f32,
        /// How far above the target's origin the camera looks.
        height: f32,
        /// Follow rate per second; higher is snappier.
        smoothing: f32,
        /// What input orbits the camera.
        orbit: OrbitInput,
        /// Lowest the camera may tilt, in degrees.
        pitch_min_degrees: f32,
        /// Highest the camera may tilt, in degrees.
        pitch_max_degrees: f32,
    },
    /// At the target's head, turned by mouse or stick.
    FirstPerson {
        /// How far above the target's origin the eye sits.
        height: f32,
        /// What input turns the camera.
        look: OrbitInput,
        /// Lowest the camera may tilt, in degrees.
        pitch_min_degrees: f32,
        /// Highest the camera may tilt, in degrees.
        pitch_max_degrees: f32,
    },
    /// Sideways 2D follow: x/y only, inside a dead zone, within bounds.
    Side2D {
        /// Half-size of the area the target can move in before the camera
        /// follows.
        dead_zone: [f32; 2],
        /// `min_x, min_y, max_x, max_y` the camera position stays within.
        bounds: Option<[f32; 4]>,
        /// Follow rate per second; higher is snappier.
        smoothing: f32,
    },
}

/// On-disk camera follow: which object the camera tracks, and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraFollowData {
    /// Stable object id of the target.
    pub target: String,
    /// How to follow it.
    pub mode: CameraFollowMode,
    /// A key that switches between first and third person in the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_switch: Option<CameraViewSwitch>,
}

/// On-disk key for switching a follow camera's view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraViewSwitch {
    /// The key's script-visible name, such as `"V"`.
    pub key: String,
    /// The view the key switches to, with its own settings.
    pub other: CameraFollowMode,
}

impl From<engine_ecs::components::OrbitInput> for OrbitInput {
    fn from(value: engine_ecs::components::OrbitInput) -> Self {
        use engine_ecs::components::OrbitInput as Live;
        match value {
            Live::None => Self::None,
            Live::Mouse => Self::Mouse,
            Live::RightStick => Self::RightStick,
            Live::Both => Self::Both,
        }
    }
}

impl From<OrbitInput> for engine_ecs::components::OrbitInput {
    fn from(value: OrbitInput) -> Self {
        match value {
            OrbitInput::None => Self::None,
            OrbitInput::Mouse => Self::Mouse,
            OrbitInput::RightStick => Self::RightStick,
            OrbitInput::Both => Self::Both,
        }
    }
}

impl From<engine_ecs::components::FollowMode> for CameraFollowMode {
    fn from(value: engine_ecs::components::FollowMode) -> Self {
        use engine_ecs::components::FollowMode as Live;
        match value {
            Live::ThirdPerson {
                distance,
                height,
                smoothing,
                orbit,
                pitch_min_degrees,
                pitch_max_degrees,
            } => Self::ThirdPerson {
                distance,
                height,
                smoothing,
                orbit: orbit.into(),
                pitch_min_degrees,
                pitch_max_degrees,
            },
            Live::FirstPerson {
                height,
                look,
                pitch_min_degrees,
                pitch_max_degrees,
            } => Self::FirstPerson {
                height,
                look: look.into(),
                pitch_min_degrees,
                pitch_max_degrees,
            },
            Live::Side2D {
                dead_zone,
                bounds,
                smoothing,
            } => Self::Side2D {
                dead_zone,
                bounds,
                smoothing,
            },
        }
    }
}

impl From<CameraFollowMode> for engine_ecs::components::FollowMode {
    fn from(value: CameraFollowMode) -> Self {
        match value {
            CameraFollowMode::ThirdPerson {
                distance,
                height,
                smoothing,
                orbit,
                pitch_min_degrees,
                pitch_max_degrees,
            } => Self::ThirdPerson {
                distance,
                height,
                smoothing,
                orbit: orbit.into(),
                pitch_min_degrees,
                pitch_max_degrees,
            },
            CameraFollowMode::FirstPerson {
                height,
                look,
                pitch_min_degrees,
                pitch_max_degrees,
            } => Self::FirstPerson {
                height,
                look: look.into(),
                pitch_min_degrees,
                pitch_max_degrees,
            },
            CameraFollowMode::Side2D {
                dead_zone,
                bounds,
                smoothing,
            } => Self::Side2D {
                dead_zone,
                bounds,
                smoothing,
            },
        }
    }
}

impl From<&engine_ecs::components::CameraFollow> for CameraFollowData {
    fn from(live: &engine_ecs::components::CameraFollow) -> Self {
        Self {
            target: live.target.clone(),
            mode: live.mode.into(),
            view_switch: live.switch.as_ref().map(|switch| CameraViewSwitch {
                key: switch.key.clone(),
                other: switch.other.into(),
            }),
        }
    }
}

impl From<CameraFollowData> for engine_ecs::components::CameraFollow {
    fn from(data: CameraFollowData) -> Self {
        let mut follow = Self::new(data.target, data.mode.into());
        follow.switch = data
            .view_switch
            .map(|switch| engine_ecs::components::ViewSwitch {
                key: switch.key,
                other: switch.other.into(),
            });
        follow
    }
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
            pose_from_transform: false,
            active: false,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshRendererData {
    /// The mesh asset this entity renders.
    pub mesh: AssetRef,
    /// The material asset this entity renders with.
    pub material: AssetRef,
    /// Optional per-object PBR changes; absent in older scenes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_override: Option<MaterialOverrideData>,
}

/// Per-object adjustments layered over the imported glTF material.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialOverrideData {
    /// Multiplies the imported base color's RGB; alpha is unchanged.
    pub tint: [f32; 3],
    /// Replaces the imported metallic factor when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    /// Replaces the imported roughness factor when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
}

impl Default for MaterialOverrideData {
    fn default() -> Self {
        Self {
            tint: [1.0; 3],
            metallic: None,
            roughness: None,
        }
    }
}

impl MaterialOverrideData {
    /// Applies this object's authored adjustments without editing the shared asset.
    pub fn apply(&self, mut material: engine_renderer::Material) -> engine_renderer::Material {
        for (color, tint) in material.base_color_factor[..3].iter_mut().zip(self.tint) {
            *color *= tint;
        }
        if let Some(metallic) = self.metallic {
            material.metallic_factor = metallic;
        }
        if let Some(roughness) = self.roughness {
            material.roughness_factor = roughness;
        }
        material
    }
}

/// Authored material adjustments on a live entity, retained even if its mesh is missing.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneMaterialOverride(pub MaterialOverrideData);

/// The adjustments used to build a live mesh; the editor compares this with
/// [`SceneMaterialOverride`] to refresh only entities whose material changed.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct AppliedMaterialOverride(pub Option<MaterialOverrideData>);

/// One authored foliage group: a model drawn at many world-space poses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoliageData {
    /// Model geometry to instance.
    pub mesh: AssetRef,
    /// Imported material carried by the model.
    pub material: AssetRef,
    /// World-space instance transforms, in deterministic scatter order.
    pub instances: Vec<TransformData>,
}

/// Live source data retained if the model cannot yet be resolved.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneFoliage(pub FoliageData);

/// On-disk sprite — the serializable, reference-only form of what will
/// resolve into a live `engine_ecs::Sprite`. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteData {
    /// The atlas texture asset this sprite samples.
    pub atlas: AssetRef,
    /// `full`, a legacy name (draws the full image), or
    /// `grid:columns,rows:column,row` for one evenly divided sheet cell.
    pub region: String,
    /// World-space width/height.
    pub size: [f32; 2],
    /// Linear RGBA tint.
    pub color: [f32; 4],
    /// Explicit 2D layer: higher draws on top. Defaults to `0.0`, which
    /// is also what every scene written before layering existed reads
    /// back as.
    #[serde(default)]
    pub z_order: f32,
    /// Optional row-major flipbook starting at `region`'s grid cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<SpriteAnimationData>,
}

/// A simple sprite-sheet flipbook. The first frame is the sprite's grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpriteAnimationData {
    /// Consecutive row-major cells, including the first one.
    pub frames: u32,
    /// Playback speed in frames per second.
    pub fps: f32,
    /// Whether playback returns to the first frame after the last.
    pub looping: bool,
}

/// Live flipbook clock; source sprite region stays at the authored first cell.
#[derive(bevy_ecs::component::Component, Debug, Clone, Copy, PartialEq)]
pub struct SceneSpriteAnimation {
    /// Authored playback settings.
    pub data: SpriteAnimationData,
    /// Seconds since playback began, bounded by the clip duration.
    pub elapsed: f32,
    /// The sprite image's rectangle inside the packed scene atlas.
    pub image_uv: Option<engine_renderer::UvRect>,
    /// Runtime playback state; not written into the authored scene.
    pub playing: bool,
}

impl SceneSpriteAnimation {
    /// Starts the flipbook at its authored first cell.
    pub fn new(data: SpriteAnimationData) -> Self {
        Self {
            data,
            elapsed: 0.0,
            image_uv: None,
            playing: true,
        }
    }
}

/// Sparse tilemap settings. Painted cells are ordinary child sprites, so
/// both editor preview and either game language use the normal sprite path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilemapData {
    /// Image split into the sheet grid.
    pub atlas: AssetRef,
    /// Sheet columns and rows, not map bounds.
    pub columns: u32,
    /// Sheet rows.
    pub rows: u32,
    /// World-space width and height of each painted tile.
    pub cell_size: [f32; 2],
    /// Current editor brush column.
    pub brush_column: u32,
    /// Current editor brush row.
    pub brush_row: u32,
    /// Whether painting a cell adds fixed collision to that cell.
    #[serde(default)]
    pub brush_collision: bool,
}

impl TilemapData {
    /// A four-by-four sprite sheet with one-world-unit tiles.
    pub fn new(atlas: AssetRef) -> Self {
        Self {
            atlas,
            columns: 4,
            rows: 4,
            cell_size: [1.0, 1.0],
            brush_column: 0,
            brush_row: 0,
            brush_collision: false,
        }
    }
}

/// Live carrier for a tilemap parent; painted children carry the sprites.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneTilemap(pub TilemapData);

/// Screen-space anchor for an authored game UI element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UiAnchor {
    /// Parent screen's top-left corner.
    #[default]
    TopLeft,
    /// Center of the top edge.
    Top,
    /// Top-right corner.
    TopRight,
    /// Center of the left edge.
    Left,
    /// Screen center.
    Center,
    /// Center of the right edge.
    Right,
    /// Bottom-left corner.
    BottomLeft,
    /// Center of the bottom edge.
    Bottom,
    /// Bottom-right corner.
    BottomRight,
}

/// The two authorable game UI widgets in this scene format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiKind {
    /// Read-only text.
    Label,
    /// Clickable text on a colored rectangle.
    Button,
}

/// One label or button, positioned in pixels relative to the game window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiElementData {
    /// Label or button.
    pub kind: UiKind,
    /// Visible caption.
    pub text: String,
    /// Screen edge or point used for placement.
    pub anchor: UiAnchor,
    /// Pixel displacement from the anchor.
    pub offset: [f32; 2],
    /// Width and height in screen pixels.
    pub size: [f32; 2],
    /// Linear RGBA text color for labels, background color for buttons.
    pub color: [f32; 4],
}

/// Carries authored UI data on a named scene entity in the editor world.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneUi(pub UiElementData);

/// Most conditions one rule may carry.
pub const MAX_RULE_CONDITIONS: usize = 8;

/// An authored condition that can drive a scene action in either game language.
///
/// Saved through [`RuleDataOnDisk`]: a rule with exactly one condition and
/// [`RuleMatch::All`] keeps the older single-`condition` form so older SDKs
/// still read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "RuleDataOnDisk", into = "RuleDataOnDisk")]
pub struct RuleData {
    /// Event that runs this rule.
    pub trigger: RuleTrigger,
    /// Engine-owned action to apply when the event arrives.
    pub action: RuleAction,
    /// Checks on gameplay variables; the action runs only when they hold
    /// (see `match_mode`). At most [`MAX_RULE_CONDITIONS`].
    pub conditions: Vec<RuleCondition>,
    /// Whether every condition or any one condition must hold.
    pub match_mode: RuleMatch,
    /// Where the Gameplay Graph draws this rule's action node, in graph units.
    /// Editor layout only; `None` uses the automatic column layout.
    pub graph_position: Option<[f32; 2]>,
}

/// How a rule combines several conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RuleMatch {
    /// Every condition must hold.
    #[default]
    All,
    /// At least one condition must hold.
    Any,
}

#[derive(Serialize, Deserialize)]
struct RuleDataOnDisk {
    trigger: RuleTrigger,
    action: RuleAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    condition: Option<RuleCondition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conditions: Vec<RuleCondition>,
    #[serde(default, skip_serializing_if = "is_match_all")]
    match_mode: RuleMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    graph_position: Option<[f32; 2]>,
}

fn is_match_all(mode: &RuleMatch) -> bool {
    *mode == RuleMatch::All
}

impl From<RuleDataOnDisk> for RuleData {
    fn from(disk: RuleDataOnDisk) -> Self {
        let mut conditions: Vec<RuleCondition> = disk.condition.into_iter().collect();
        conditions.extend(disk.conditions);
        Self {
            trigger: disk.trigger,
            action: disk.action,
            conditions,
            match_mode: disk.match_mode,
            graph_position: disk.graph_position,
        }
    }
}

impl From<RuleData> for RuleDataOnDisk {
    fn from(rule: RuleData) -> Self {
        let legacy = rule.conditions.len() == 1 && rule.match_mode == RuleMatch::All;
        let (condition, conditions, match_mode) = if legacy {
            (
                rule.conditions.into_iter().next(),
                Vec::new(),
                RuleMatch::All,
            )
        } else {
            (None, rule.conditions, rule.match_mode)
        };
        Self {
            trigger: rule.trigger,
            action: rule.action,
            condition,
            conditions,
            match_mode,
            graph_position: rule.graph_position,
        }
    }
}

impl RuleData {
    /// An unconditional rule with automatic graph layout.
    pub fn new(trigger: RuleTrigger, action: RuleAction) -> Self {
        Self {
            trigger,
            action,
            conditions: Vec::new(),
            match_mode: RuleMatch::All,
            graph_position: None,
        }
    }

    /// Whether the rule's conditions allow its action to run now. An empty
    /// list always holds.
    pub fn conditions_hold(&self, variables: &crate::RuleVariables) -> bool {
        let holds = |condition: &RuleCondition| {
            condition
                .compare
                .holds(variables.get(&condition.variable), condition.value)
        };
        if self.conditions.is_empty() {
            return true;
        }
        match self.match_mode {
            RuleMatch::All => self.conditions.iter().all(holds),
            RuleMatch::Any => self.conditions.iter().any(holds),
        }
    }
}

/// Compares a numeric gameplay variable with a constant. Unset variables read as `0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleCondition {
    /// Variable name, using the same 1–64 printable-byte rule as event names.
    pub variable: String,
    /// How the variable is compared with `value`.
    pub compare: RuleCompare,
    /// Finite constant on the right-hand side.
    pub value: f64,
}

/// Comparison used by a [`RuleCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleCompare {
    /// `variable == value`
    Equal,
    /// `variable != value`
    NotEqual,
    /// `variable < value`
    Less,
    /// `variable <= value`
    LessOrEqual,
    /// `variable > value`
    Greater,
    /// `variable >= value`
    GreaterOrEqual,
}

impl RuleCompare {
    /// Every comparison, in editor display order.
    pub const ALL: [Self; 6] = [
        Self::Equal,
        Self::NotEqual,
        Self::Less,
        Self::LessOrEqual,
        Self::Greater,
        Self::GreaterOrEqual,
    ];

    /// Short operator text for editor labels.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "≠",
            Self::Less => "<",
            Self::LessOrEqual => "≤",
            Self::Greater => ">",
            Self::GreaterOrEqual => "≥",
        }
    }

    /// Applies the comparison.
    pub fn holds(self, left: f64, right: f64) -> bool {
        match self {
            Self::Equal => left == right,
            Self::NotEqual => left != right,
            Self::Less => left < right,
            Self::LessOrEqual => left <= right,
            Self::Greater => left > right,
            Self::GreaterOrEqual => left >= right,
        }
    }
}

/// A click on this rule's owning button, or a gameplay event name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuleTrigger {
    /// The owning UI Button was clicked.
    UiClicked,
    /// A Lua or Rust game emitted this name.
    Named(String),
    /// Once per scene load, after that scene's scripts' `start`.
    SceneStart,
    /// The named key went down this frame.
    KeyPressed {
        /// Key name, as used by the input API.
        key: String,
    },
    /// This object began touching another solid object.
    CollisionEnter {
        /// Stable ID of the other object; `None` matches any.
        with: Option<String>,
    },
    /// This object stopped touching another solid object.
    CollisionExit {
        /// Stable ID of the other object; `None` matches any.
        with: Option<String>,
    },
    /// This object overlapped a trigger volume or another object began overlapping its trigger.
    TriggerEnter {
        /// Stable ID of the other object; `None` matches any.
        with: Option<String>,
    },
    /// An overlap with a trigger volume ended.
    TriggerExit {
        /// Stable ID of the other object; `None` matches any.
        with: Option<String>,
    },
    /// A countdown finished.
    Timer {
        /// Seconds until it fires, in `0.05..=3600`.
        seconds: f32,
        /// Whether it starts again after firing.
        repeat: bool,
    },
    /// A gameplay variable changed value.
    VariableChanged {
        /// Variable name.
        name: String,
    },
    /// The game state changed to this state.
    StateChanged {
        /// The state entered.
        state: crate::GameStateName,
    },
}

/// Where a spawned prefab appears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SpawnAt {
    /// At the position of the object with this stable ID.
    Object(String),
    /// At a fixed world position.
    Position([f32; 3]),
}

/// Actions available to visual rules without exposing engine internals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuleAction {
    /// Replace an authored UI Label or Button's caption.
    SetUiText {
        /// Stable ID of an authored UI Label or Button.
        target: String,
        /// Replacement caption.
        text: String,
    },
    /// Trigger an authored audio emitter.
    PlayAudio {
        /// Stable ID of an authored audio emitter.
        target: String,
    },
    /// Queue another named event after the current event's actions.
    EmitEvent {
        /// Name of the event to send to other authored rules.
        name: String,
    },
    /// Set a numeric gameplay variable.
    SetVariable {
        /// Variable name.
        name: String,
        /// Finite new value.
        value: f64,
    },
    /// Add to a numeric gameplay variable; use a negative amount to subtract.
    AddToVariable {
        /// Variable name; an unset variable starts at `0`.
        name: String,
        /// Finite amount to add.
        amount: f64,
    },
    /// Load another scene.
    ChangeScene {
        /// Project-relative `scenes/*.ron` path.
        path: String,
    },
    /// Show or hide an object.
    SetVisible {
        /// Stable ID of the object.
        target: String,
        /// Whether it is drawn.
        visible: bool,
    },
    /// Enable or disable an object.
    SetEnabled {
        /// Stable ID of the object.
        target: String,
        /// Whether it is active.
        enabled: bool,
    },
    /// Create an object from a prefab.
    SpawnPrefab {
        /// Project-relative `assets/prefabs/*.prefab` path.
        path: String,
        /// Where it appears.
        at: SpawnAt,
    },
    /// Remove an object from the scene.
    Destroy {
        /// Stable ID of the object.
        target: String,
    },
    /// Switch the game state.
    SetGameState {
        /// The state to enter.
        state: crate::GameStateName,
    },
}

/// Live carrier for an entity's saved visual rules.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneRules(pub Vec<RuleData>);

/// The physics shape a [`SceneEntity`] collides with.
///
/// Primitive shapes carry their own dimensions in local space, scaled by
/// the entity's transform when the body is built.
/// [`ColliderShape::TriMesh`] instead points at a mesh asset and
/// collides against its actual triangles — the right choice for terrain,
/// where an approximation would let the player fall through a hill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ColliderShape {
    /// A sphere of `radius`.
    Ball {
        /// Radius in local units.
        radius: f32,
    },
    /// A box, given as half-extents on each axis.
    Cuboid {
        /// Half-width, half-height, half-depth.
        half_extents: [f32; 3],
    },
    /// A Y-axis cylinder.
    Cylinder {
        /// Half the total height.
        half_height: f32,
        /// Radius in local units.
        radius: f32,
    },
    /// A Y-axis capsule — a cylinder with hemispherical caps. The usual
    /// shape for a character.
    Capsule {
        /// Half the height of the cylindrical section.
        half_height: f32,
        /// Radius in local units.
        radius: f32,
    },
    /// Every triangle of a mesh asset.
    ///
    /// The reference is a mesh asset id, normally the same one the
    /// entity renders, so what you see and what you collide with cannot
    /// drift apart.
    TriMesh {
        /// The mesh asset to collide against.
        mesh: AssetRef,
    },
}

/// How a [`SceneEntity`]'s body behaves in the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyKind {
    /// Never moves; the whole static world.
    #[default]
    Fixed,
    /// Moved by forces and collisions.
    Dynamic,
    /// Moved by game code, pushing others aside.
    Kinematic,
}

/// One entity's physics body: a shape plus how it moves.
///
/// The scene format carries this as *data* rather than leaving every
/// game to rebuild its collision in code. Without it a loaded scene is
/// scenery you walk through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColliderData {
    /// The collision shape.
    pub shape: ColliderShape,
    /// How the body moves.
    #[serde(default)]
    pub body: BodyKind,
    /// A trigger detects overlaps but never blocks anything.
    #[serde(default)]
    pub trigger: bool,
    /// Mass in kilograms, or `None` for Auto (from the shape's volume).
    /// Only Dynamic bodies use it.
    #[serde(default)]
    pub mass: Option<f32>,
    /// Surface grip, `0` is ice.
    #[serde(default = "default_friction")]
    pub friction: f32,
    /// How much of its speed a hit gives back, `0` to `1`.
    #[serde(default)]
    pub bounciness: f32,
    /// Multiplier on gravity. Only Dynamic bodies use it.
    #[serde(default = "default_gravity_scale")]
    pub gravity_scale: f32,
    /// Keep the body upright; collisions cannot turn it.
    #[serde(default)]
    pub lock_rotation: bool,
}

impl ColliderData {
    /// A collider with every setting at its default.
    pub fn new(shape: ColliderShape, body: BodyKind) -> Self {
        Self {
            shape,
            body,
            trigger: false,
            mass: None,
            friction: default_friction(),
            bounciness: 0.0,
            gravity_scale: default_gravity_scale(),
            lock_rotation: false,
        }
    }
}

fn default_friction() -> f32 {
    0.5
}

fn default_gravity_scale() -> f32 {
    1.0
}

/// A scene entity's collider, carried on the live entity so the editor
/// does not lose it.
///
/// The same rule as [`engine_ecs::components::MeshSource`]: an editor
/// world holds no physics, so without a carrier component the collider a
/// scene file declares would exist only until the next save — 166 bodies
/// in the island's scene would vanish the first time somebody moved a
/// rock and pressed Save. A runtime builds real rapier colliders from
/// this same data and ignores the component.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneCollider(pub ColliderData);

/// A sound attached to an entity, as the scene stores it.
///
/// The live `AudioEmitter` owns a decoded `StaticSound`, which is not
/// something a text file can hold — this is the authorable half: which
/// asset, how loud, how far it carries, whether it starts on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioEmitterData {
    /// The sound asset to play.
    pub sound: AssetRef,
    /// Start playing as soon as the entity is spawned.
    #[serde(default = "default_true")]
    pub autoplay: bool,
    /// Loop the clip instead of playing it once.
    #[serde(default)]
    pub looping: bool,
    /// Linear gain, `1.0` being the clip's own level.
    #[serde(default = "default_gain")]
    pub gain: f32,
    /// How far the sound carries, in world units — drawn by the
    /// Scene-view overlay so a sound's reach is visible while placing it.
    #[serde(default = "default_radius")]
    pub radius: f32,
}

fn default_true() -> bool {
    true
}

fn default_gain() -> f32 {
    1.0
}

fn default_radius() -> f32 {
    10.0
}

impl AudioEmitterData {
    /// An autoplaying, non-looping emitter for `sound` at unit gain.
    pub fn new(sound: AssetRef) -> Self {
        Self {
            sound,
            autoplay: true,
            looping: false,
            gain: default_gain(),
            radius: default_radius(),
        }
    }
}

/// An entity's audio emitter, carried on the live entity so the editor
/// can show, edit and re-save it.
///
/// Same carrier rule as [`SceneCollider`]: the live emitter holds a
/// decoded sound and can only exist where audio can be loaded, so
/// without this the emitter would vanish from any scene the editor
/// saved.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneAudioEmitter(pub AudioEmitterData);

/// A walkability grid authored with the scene, in the shape
/// `engine_ai`'s pathfinder wants.
///
/// Scene-level rather than per-entity: it describes the *level*, not a
/// thing in it. Cells are row-major, `width * height`, `true` meaning
/// blocked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavGridData {
    /// Cells along X.
    pub width: u32,
    /// Cells along Z.
    pub height: u32,
    /// World units per cell.
    pub cell_size: f32,
    /// World-space `(x, z)` of cell `(0, 0)`'s minimum corner.
    pub origin: [f32; 2],
    /// Row-major blocked flags, `width * height` of them. Any other
    /// length is a malformed grid — see [`Scene::validate`].
    pub blocked: Vec<bool>,
}

impl NavGridData {
    /// A fully walkable grid.
    pub fn new(width: u32, height: u32, cell_size: f32, origin: [f32; 2]) -> Self {
        Self {
            width,
            height,
            cell_size,
            origin,
            blocked: vec![false; (width as usize) * (height as usize)],
        }
    }

    /// How many cells the grid declares.
    pub fn cell_count(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Whether `blocked` carries exactly one flag per cell.
    pub fn is_well_formed(&self) -> bool {
        self.blocked.len() == self.cell_count()
    }
}

/// The scene's nav grid, held as a world resource while the scene is
/// open so the editor can draw it and save it back.
#[derive(bevy_ecs::resource::Resource, Debug, Clone, PartialEq)]
pub struct SceneNavGrid(pub NavGridData);

/// Stable display position among siblings in the editor hierarchy.
#[derive(bevy_ecs::component::Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiblingOrder(pub u64);

/// Stable authored identity, independent of the ECS entity allocated on load.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq, Eq)]
pub struct SceneObjectId(pub String);

/// Authored scene-wide daylight. Absent in older scenes, which retain the
/// host application's existing lighting defaults.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LightingData {
    /// Direction the sunlight travels toward, in world space.
    pub sun_direction: [f32; 3],
    /// Linear RGB sunlight color.
    pub sun_color: [f32; 3],
    /// Sunlight brightness.
    pub sun_intensity: f32,
    /// Brightness of the renderer's daylight sky/ground ambient colors.
    pub ambient_intensity: f32,
}

impl Default for LightingData {
    fn default() -> Self {
        Self {
            sun_direction: [-0.4, -1.0, -0.3],
            sun_color: [1.0, 0.98, 0.92],
            sun_intensity: 3.0,
            ambient_intensity: 1.0,
        }
    }
}

impl LightingData {
    /// Converts validated authored values to the shared renderer light set.
    pub fn to_light_set(self) -> engine_renderer::LightSet {
        let mut lights = engine_renderer::LightSet::new();
        lights.directional.push(engine_renderer::DirectionalLight {
            direction: glam::Vec3::from_array(self.sun_direction).normalize(),
            color: glam::Vec3::from_array(self.sun_color),
            intensity: self.sun_intensity,
        });
        lights.ambient.intensity = self.ambient_intensity;
        lights
    }

    pub(crate) fn validate(self) -> Result<(), String> {
        let direction = glam::Vec3::from_array(self.sun_direction);
        if !direction.is_finite()
            || !direction.length_squared().is_finite()
            || direction.length_squared() < 0.000001
        {
            return Err("sun_direction must be finite, normalizable, and nonzero".into());
        }
        if !self
            .sun_color
            .iter()
            .all(|value| value.is_finite() && (0.0..=100.0).contains(value))
        {
            return Err("sun_color must contain finite values in [0, 100]".into());
        }
        if !(self.sun_intensity.is_finite() && (0.0..=1000.0).contains(&self.sun_intensity)) {
            return Err("sun_intensity must be finite and in [0, 1000]".into());
        }
        if !(self.ambient_intensity.is_finite() && (0.0..=1000.0).contains(&self.ambient_intensity))
        {
            return Err("ambient_intensity must be finite and in [0, 1000]".into());
        }
        Ok(())
    }
}

/// Scene-wide settings that have no position in the world, such as ambient
/// wind or background music. Positional effects (a waterfall, a torch)
/// stay components on the objects that make them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EnvironmentData {
    /// Sounds that loop for the whole scene at the same volume everywhere.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambient_sounds: Vec<AmbientSoundData>,
    /// State the game starts in; `None` means the default, playing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_state: Option<crate::GameStateName>,
}

impl EnvironmentData {
    /// Most ambient sounds one scene may declare.
    pub const MAX_AMBIENT_SOUNDS: usize = 8;

    /// Whether nothing is set, so the scene file can omit the section.
    pub fn is_empty(&self) -> bool {
        self.ambient_sounds.is_empty() && self.start_state.is_none()
    }
}

/// One looping, non-positional scene sound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmbientSoundData {
    /// The audio asset to loop.
    pub sound: AssetRef,
    /// Linear gain, `1.0` being the clip's own level (at most `4.0`).
    #[serde(default = "default_gain")]
    pub gain: f32,
}

/// ECS carrier for the scene's [`EnvironmentData`] while editing.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, PartialEq, Default)]
pub struct SceneEnvironment(pub EnvironmentData);

/// ECS carrier for scene-wide authored lighting.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, Copy, PartialEq)]
pub struct SceneLighting(pub LightingData);

/// Editable light properties; position comes from the entity transform.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PointLightData {
    /// Linear RGB color.
    pub color: [f32; 3],
    /// Brightness multiplier.
    pub intensity: f32,
    /// Editor reach indicator and future falloff distance.
    pub range: f32,
}

impl Default for PointLightData {
    fn default() -> Self {
        Self {
            color: [1.0, 0.9, 0.7],
            intensity: 10.0,
            range: 8.0,
        }
    }
}

/// ECS carrier for a scene entity's authored point light.
#[derive(bevy_ecs::prelude::Component, Debug, Clone, Copy, PartialEq)]
pub struct ScenePointLight(pub PointLightData);

/// Serializable water settings; the live surface is validated on load.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WaterData {
    /// World-space centre of the XZ rectangle.
    pub center: [f32; 3],
    /// Positive half-width and half-depth.
    pub half_extent: [f32; 2],
    /// Still-water height.
    pub base_height: f32,
    /// Wave travel direction in XZ.
    pub wave_direction: [f32; 2],
    /// Wave height.
    pub wave_amplitude: f32,
    /// Crest-to-crest distance.
    pub wave_length: f32,
    /// Angular wave speed.
    pub wave_speed: f32,
    /// Surface flow velocity.
    pub current: [f32; 3],
    /// Water mass density.
    pub density: f32,
    /// Linear water drag.
    pub linear_drag: f32,
}

impl From<engine_physics::WaterSurface> for WaterData {
    fn from(surface: engine_physics::WaterSurface) -> Self {
        Self {
            center: surface.center.to_array(),
            half_extent: surface.half_extent.to_array(),
            base_height: surface.base_height,
            wave_direction: surface.wave.direction.to_array(),
            wave_amplitude: surface.wave.amplitude,
            wave_length: surface.wave.length,
            wave_speed: surface.wave.speed,
            current: surface.current.to_array(),
            density: surface.density,
            linear_drag: surface.linear_drag,
        }
    }
}

impl TryFrom<WaterData> for engine_physics::WaterSurface {
    type Error = engine_physics::PhysicsError;

    fn try_from(data: WaterData) -> Result<Self, Self::Error> {
        Self::new(
            glam::Vec3::from_array(data.center),
            glam::Vec2::from_array(data.half_extent),
            data.base_height,
            engine_physics::WaterWave {
                direction: glam::Vec2::from_array(data.wave_direction),
                amplitude: data.wave_amplitude,
                length: data.wave_length,
                speed: data.wave_speed,
            },
            glam::Vec3::from_array(data.current),
            data.density,
            data.linear_drag,
        )
    }
}

/// One entity's serializable components. Every field is optional — an
/// entity may carry any subset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneEntity {
    /// Stable UUID for gameplay references; old scenes receive one on first save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    /// Project-relative source attached to this entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
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
    /// Optional display position among siblings. Old scenes without it
    /// keep their existing fallback order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sibling_order: Option<u64>,
    /// This entity's transform, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<TransformData>,
    /// This entity's camera, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraData>,
    /// Makes this camera follow another object, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_follow: Option<CameraFollowData>,
    /// This entity's mesh/material, as asset references, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_renderer: Option<MeshRendererData>,
    /// Authored instanced foliage group, absent in older scenes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foliage: Option<FoliageData>,
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
    /// Sparse 2D tilemap settings; cells are normal child sprite entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilemap: Option<TilemapData>,
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
    /// This entity's physics body, if it has one. Scene format v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collider: Option<ColliderData>,
    /// A sound this entity emits, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_emitter: Option<AudioEmitterData>,
    /// Authored water surface; absent in older scenes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water: Option<WaterData>,
    /// Authored point light; its position is the entity's transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_light: Option<PointLightData>,
    /// Authored screen-space UI label or button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiElementData>,
    /// Event/action rules owned by this object; absent in older scenes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RuleData>,
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
    /// The level's walkability grid, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_grid: Option<NavGridData>,
    /// Scene-wide authored daylight, if this scene overrides host defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lighting: Option<LightingData>,
    /// Scene-wide effects with no position: ambient sounds.
    #[serde(default, skip_serializing_if = "EnvironmentData::is_empty")]
    pub environment: EnvironmentData,
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
            nav_grid: None,
            lighting: None,
            environment: EnvironmentData::default(),
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
        let mut scene = crate::migration::migrate(scene)?;
        crate::camera_pose::adopt_legacy_camera_poses(&mut scene);
        Ok(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn follow_scene(mode: CameraFollowMode) -> Scene {
        let camera = CameraData::from(engine_renderer::Camera::new(
            glam::Vec3::ZERO,
            glam::Vec3::NEG_Z,
            1.5,
        ));
        Scene {
            entities: vec![SceneEntity {
                camera: Some(camera),
                camera_follow: Some(CameraFollowData {
                    target: "player-id".into(),
                    mode,
                    view_switch: None,
                }),
                ..Default::default()
            }],
            ..Scene::default()
        }
    }

    #[test]
    fn camera_follow_round_trips_and_validates() {
        for mode in [
            engine_ecs::components::FollowMode::THIRD_PERSON.into(),
            engine_ecs::components::FollowMode::FIRST_PERSON.into(),
            CameraFollowMode::Side2D {
                dead_zone: [1.0, 0.5],
                bounds: Some([-5.0, 0.0, 20.0, 8.0]),
                smoothing: 4.0,
            },
        ] {
            let scene = follow_scene(mode);
            assert!(scene.validate().is_ok());
            let text = scene.to_ron_string().unwrap();
            let back = Scene::from_ron_str(&text).unwrap();
            assert_eq!(
                back.entities[0].camera_follow,
                scene.entities[0].camera_follow
            );
        }
        // Old scenes have no follow.
        let old = Scene::from_ron_str("(entities: [(camera: None)])").unwrap();
        assert!(old.entities[0].camera_follow.is_none());
    }

    #[test]
    fn camera_follow_survives_world_capture() {
        let scene = follow_scene(engine_ecs::components::FollowMode::SIDE_2D.into());
        let mut world = bevy_ecs::prelude::World::new();
        let mut named = scene.clone();
        named.entities[0].name = Some("Cam".into());
        named.instantiate(&mut world);
        let captured = Scene::from_world(&mut world);
        assert_eq!(
            captured.entities[0].camera_follow,
            scene.entities[0].camera_follow
        );
    }

    #[test]
    fn camera_follow_rejects_bad_values() {
        let bad = |mode: CameraFollowMode| follow_scene(mode).validate().is_err();
        let first = |height, min, max| CameraFollowMode::FirstPerson {
            height,
            look: OrbitInput::Mouse,
            pitch_min_degrees: min,
            pitch_max_degrees: max,
        };
        assert!(!bad(first(1.6, -85.0, 85.0)));
        assert!(bad(first(f32::NAN, -85.0, 85.0)));
        assert!(bad(first(1.6, -95.0, 85.0)));
        assert!(bad(first(1.6, 10.0, -10.0)));
        let third = |distance, smoothing, min, max| CameraFollowMode::ThirdPerson {
            distance,
            height: 1.0,
            smoothing,
            orbit: OrbitInput::Mouse,
            pitch_min_degrees: min,
            pitch_max_degrees: max,
        };
        assert!(!bad(third(6.0, 10.0, -60.0, 70.0)));
        assert!(bad(third(0.0, 10.0, -60.0, 70.0)));
        assert!(bad(third(f32::NAN, 10.0, -60.0, 70.0)));
        assert!(bad(third(6.0, -1.0, -60.0, 70.0)));
        assert!(bad(third(6.0, 10.0, 70.0, -60.0)));
        assert!(bad(third(6.0, 10.0, -60.0, 90.0)));
        let side = |dead_zone, bounds, smoothing| CameraFollowMode::Side2D {
            dead_zone,
            bounds,
            smoothing,
        };
        assert!(!bad(side([0.0, 0.0], Some([0.0, 0.0, 0.0, 0.0]), 0.0)));
        assert!(bad(side([-1.0, 0.0], None, 1.0)));
        assert!(bad(side([f32::INFINITY, 0.0], None, 1.0)));
        assert!(bad(side([1.0, 1.0], Some([5.0, 0.0, 1.0, 1.0]), 1.0)));
        assert!(bad(side([1.0, 1.0], Some([0.0, 0.0, f32::NAN, 1.0]), 1.0)));
        assert!(bad(side([1.0, 1.0], None, f32::NAN)));
    }

    #[test]
    fn old_collider_data_loads_with_defaults() {
        let old: ColliderData =
            ron::from_str("(shape: Cuboid(half_extents: (0.5,0.5,0.5)), body: Dynamic)").unwrap();
        assert!(!old.trigger);
        assert_eq!(old.mass, None);
        assert_eq!(old.friction, 0.5);
        assert_eq!(old.bounciness, 0.0);
        assert_eq!(old.gravity_scale, 1.0);
        assert!(!old.lock_rotation);
        assert_eq!(
            old,
            ColliderData::new(
                ColliderShape::Cuboid {
                    half_extents: [0.5; 3]
                },
                BodyKind::Dynamic
            )
        );
    }

    #[test]
    fn collider_settings_round_trip() {
        let data = ColliderData {
            shape: ColliderShape::Ball { radius: 2.0 },
            body: BodyKind::Kinematic,
            trigger: true,
            mass: Some(4.5),
            friction: 0.9,
            bounciness: 0.3,
            gravity_scale: -0.5,
            lock_rotation: true,
        };
        let text = ron::to_string(&data).unwrap();
        let back: ColliderData = ron::from_str(&text).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn tilemap_solid_brush_survives_scene_format_round_trip() {
        let source = r#"(atlas:(id:"67d09e47-5520-4b5a-a132-a68a4a60f452"),columns:4,rows:4,cell_size:(1.0,1.0),brush_column:0,brush_row:0,brush_collision:true)"#;
        let parsed: TilemapData = ron::from_str(source).unwrap();
        let saved = ron::to_string(&parsed).unwrap();
        assert!(saved.contains("brush_collision:true"));
        let legacy = source.replace(",brush_collision:true", "");
        let legacy: TilemapData = ron::from_str(&legacy).unwrap();
        assert!(!legacy.brush_collision);
    }

    #[test]
    fn material_override_tints_imported_color_and_replaces_selected_pbr_factors() {
        let authored = MaterialOverrideData {
            tint: [0.5, 0.25, 1.0],
            metallic: Some(0.8),
            roughness: None,
        };
        let imported = engine_renderer::Material {
            base_color_factor: [0.8, 0.4, 0.2, 0.7],
            metallic_factor: 0.1,
            roughness_factor: 0.65,
            ..engine_renderer::Material::DEFAULT
        };
        let applied = authored.apply(imported);
        assert_eq!(applied.base_color_factor, [0.4, 0.1, 0.2, 0.7]);
        assert_eq!(applied.metallic_factor, 0.8);
        assert_eq!(applied.roughness_factor, 0.65);
    }
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
                    transform: Some(TransformData::from(
                        engine_utils::Transform::from_translation(Vec3::new(0.0, 1.0, 5.0)),
                    )),
                    camera: Some(CameraData {
                        pose_from_transform: true,
                        ..CameraData::from(engine_renderer::Camera::new(
                            Vec3::new(0.0, 1.0, 5.0),
                            Vec3::ZERO,
                            16.0 / 9.0,
                        ))
                    }),
                    mesh_renderer: Some(MeshRendererData {
                        mesh: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                        material: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                        material_override: None,
                    }),
                    sprite: Some(SpriteData {
                        atlas: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                        region: "0_0".to_string(),
                        size: [1.0, 1.0],
                        color: [1.0, 1.0, 1.0, 1.0],
                        z_order: 0.0,
                        animation: None,
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
    fn sibling_order_in_scene_text_survives_a_save() {
        let ron = format!(
            "(version:{},entities:[(name:Some(\"First\"),sibling_order:Some(2))])",
            crate::CURRENT_SCENE_VERSION
        );
        let scene = Scene::from_ron_str(&ron).expect("scene with sibling order");
        let saved = scene.to_ron_string().expect("save scene");
        assert!(saved.contains("sibling_order: Some(2)"));
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
    #[test]
    fn sprite_without_a_z_order_reads_as_layer_zero() {
        // Every scene file written before layering existed omits the
        // field; it must load as the default rather than failing.
        let text = r#"(
            atlas: (id: "3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            region: "coin_0",
            size: (1.0, 1.0),
            color: (1.0, 1.0, 1.0, 1.0),
        )"#;
        let sprite: SpriteData = ron::from_str(text).expect("legacy sprite data must still load");
        assert_eq!(sprite.z_order, 0.0);
        assert!(sprite.animation.is_none());
    }
    #[test]
    fn sprite_animation_survives_scene_text_round_trip() {
        let text = r#"(
            atlas: (id: "3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            region: "grid:4,1:0,0",
            size: (1.0, 1.0),
            color: (1.0, 1.0, 1.0, 1.0),
            animation: Some((frames: 4, fps: 8.0, looping: true)),
        )"#;
        let sprite: SpriteData = ron::from_str(text).unwrap();
        let saved = ron::to_string(&sprite).unwrap();
        assert!(saved.contains("animation:"), "{saved}");
    }

    #[test]
    fn authored_event_rule_survives_scene_text_round_trip() {
        let text = r#"(
            name: Some("Button"),
            rules: [(
                trigger: UiClicked,
                action: PlayAudio(target: "3fa85f64-5717-4562-b3fc-2c963f66afa6"),
            )],
        )"#;
        let entity: SceneEntity = ron::from_str(text).unwrap();
        let saved = ron::to_string(&entity).unwrap();
        assert!(saved.contains("rules:"), "{saved}");
    }
    #[test]
    fn authored_lighting_round_trips_and_legacy_scene_keeps_no_override() {
        let old = Scene::from_ron_str("(version: 5, entities: [])").unwrap();
        assert!(old.lighting.is_none());

        let scene = Scene {
            lighting: Some(LightingData {
                sun_direction: [-0.3, -1.0, 0.2],
                sun_color: [1.0, 0.8, 0.6],
                sun_intensity: 2.5,
                ambient_intensity: 0.4,
            }),
            ..Scene::default()
        };
        let restored = Scene::from_ron_str(&scene.to_ron_string().unwrap()).unwrap();
        assert_eq!(restored.lighting, scene.lighting);
        #[derive(serde::Deserialize)]
        struct PreviousSceneShape {
            version: u32,
            entities: Vec<SceneEntity>,
        }
        let older: PreviousSceneShape = ron::from_str(&scene.to_ron_string().unwrap()).unwrap();
        assert_eq!(older.version, crate::CURRENT_SCENE_VERSION);
        assert!(older.entities.is_empty());
    }

    #[test]
    fn authored_point_light_survives_a_scene_text_round_trip() {
        let text = r#"(version: 5, entities: [(
            name: Some("Lamp"),
            point_light: Some((color: (1.0, 0.5, 0.25), intensity: 3.0, range: 5.0)),
        )])"#;
        let parsed = Scene::from_ron_str(text).expect("point light scene");
        let serialized = parsed.to_ron_string().expect("save scene");
        assert!(
            serialized.contains("point_light"),
            "light settings were lost"
        );
    }

    #[test]
    fn scene_rejects_invalid_authored_lighting() {
        let mut scene = Scene {
            lighting: Some(LightingData::default()),
            ..Scene::default()
        };
        scene.lighting.as_mut().unwrap().sun_direction = [0.0; 3];
        assert!(scene.validate().is_err());
        scene.lighting.as_mut().unwrap().sun_direction = [0.0, -1.0, 0.0];
        scene.lighting.as_mut().unwrap().sun_intensity = f32::NAN;
        assert!(scene.validate().is_err());
        scene.lighting.as_mut().unwrap().sun_intensity = f32::MAX;
        assert!(scene.validate().is_err());
        scene.lighting.as_mut().unwrap().sun_intensity = 1.0;
        scene.lighting.as_mut().unwrap().sun_direction = [f32::MAX, -1.0, 0.0];
        assert!(scene.validate().is_err());
    }
}
