//! # engine_scene
//!
//! Scene system: loading, saving, prefabs, entity hierarchy, and scene switching.
//!
//! ## Status
//!
//! Stage 2 "Scene format v2" complete: the [`Scene`] RON format
//! ([`TransformData`], [`CameraData`], [`SceneEntity`]) — deliberately a
//! separate on-disk shape from the live ECS component types (see the
//! module-level docs on the `format` module in source for why) — is
//! versioned ([`CURRENT_SCENE_VERSION`], migrated on parse by
//! [`Scene::from_ron_str`]), and [`SceneEntity`] carries a name, a
//! parent-index for hierarchy, and a [`MeshRendererData`] asset reference,
//! on top of the original transform/camera. File I/O
//! ([`Scene::save_to_file`], [`Scene::load_from_file`]) and semantic
//! validation ([`Scene::validate`]) cover all of it, including hierarchy
//! bounds/acyclicity — untrusted scene data can't describe a parent cycle
//! and have it survive validation. Live parent/child *transform*
//! propagation is the `engine_ecs` side (`engine_ecs::propagate_transforms`,
//! `bevy_ecs::hierarchy::ChildOf`); turning a `Scene`'s index-based
//! hierarchy into that live form is Stage 4's "Scene save/load — on the
//! Stage 2 format, from the editor." [`Prefab`] wraps the same
//! [`SceneEntity`] shape as a reusable entity template, instantiable into
//! a live `World` any number of times.

mod active_camera;
mod ai;
mod animator;
mod behaviour_tree;
mod camera_pose;
mod capture;
mod environment;
mod error;
mod format;
mod game_state;
mod io;
mod migration;
mod painted;
mod prefab;
mod prefab_link;
mod resolve;
mod road;
mod rules;
mod runtime;
mod save;
mod stream_area;
mod sprite_atlas;
mod terrain;
mod ui;
mod validate;
pub use validate::{MAX_TAG_BYTES, MAX_TAGS, valid_prefab_path, valid_scene_path};
mod world;

pub use ai::{
    AiBrainData, AiClips, AiData, AiState, MAX_AI_DISTANCE, MAX_AI_NAME_BYTES, MAX_PATROL_POINTS,
    NavAgentData, NavBakeSettings, SceneAi, SensesData, SimpleBrainData,
};
pub use behaviour_tree::{
    BehaviourTreeData, BtNodeData, BtNodeKind, MAX_BT_CHILDREN, MAX_BT_NODES,
};
pub use animator::{
    AnimatorData, AnimatorMotion, AnimatorState, AnimatorStateData, AnimatorTransitionData,
    AnimatorValues, BlendClipData, MAX_ANIMATOR_STATES, MAX_ANIMATOR_TRANSITIONS, MAX_BLEND_CLIPS,
    MAX_TRANSITION_FADE, default_position as default_animator_position,
    valid_name as valid_animator_state_name,
};
pub use active_camera::{choose_active_camera, game_camera_view, view_from_world};
pub use camera_pose::{adopt_legacy_camera_poses, camera_world_pose, look_rotation};
pub use capture::capture_renderables;
pub use environment::{AmbientSound, spawn_ambient_sounds};
pub use error::SceneError;
pub use terrain::{
    MAX_TERRAIN_LAYER_NAME_BYTES, SceneTerrain, TERRAIN_DETAIL_RANGE, TERRAIN_FILE_VERSION,
    TerrainAsset, TerrainData, TerrainLayerData, TerrainTileData, TerrainToolStateData,
    default_layers, load_surface, load_tile_file, migrate_legacy_terrains, save_tile_file, splat_paths,
    splat_relative_paths, terrain_layer_images, tile_from_bytes,
};
pub use format::{
    AmbientSoundData, AppliedLod, AppliedMaterialOverride, AssetRef, AudioEmitterData, BodyKind,
    CameraData, CameraFollowData, CameraFollowMode, ColliderData, ColliderShape, EnvironmentData,
    FoliageData, KeepBetweenScenes, LightingData, LodData, MAX_RULE_CONDITIONS,
    MaterialOverrideData, MeshRendererData, ModelAnimationData, NavGridData, OrbitInput,
    PointLightData, PrefabLink, ProjectionData, RuleAction, RuleCompare, RuleCondition, RuleData,
    RuleMatch, RuleTrigger, Scene, SceneAudioEmitter, SceneCollider, SceneEntity, SceneEnvironment,
    SceneFileOwner, SceneFoliage, SceneLighting, SceneMaterialOverride, SceneModelAnimation,
    SceneNavGrid, SceneObjectId, ScenePointLight, ScenePrefabLink, SceneRules,
    SceneSpotLight, SpotLightData, MAX_SPOT_ANGLE_DEGREES, SHADOW_DISTANCE_RANGE, FogData, CloudData,
    TimeOfDayData, MAX_DAY_MINUTES,
    SceneSpriteAnimation, SceneSpriteClips, SceneTags, SceneTilemap, SceneUi, SiblingOrder,
    SpawnAt, SpriteAnimationData, SpriteClipData, SpriteClipPlayback, SpriteData, TilemapData,
    TransformData, UiAnchor, UiElementData, UiKind, UiLayoutDirection, WaterData,
};
pub use game_state::{GameState, GameStateName};
pub use io::write_atomic;
pub use migration::CURRENT_SCENE_VERSION;
pub use prefab::Prefab;
pub use prefab_link::{
    apply_copy, changed_fields, copy_changes, copy_root_of, refresh_copies,
    remap_object_references, revert_copy, stamp_overrides,
};
pub use resolve::{
    InstantiateReport, NullResolver, SceneResolver, SkinnedModel, animation_player_for,
    insert_skinned_model, resolve_foliage,
};
pub use rules::{
    AiRuleEvent, ContactRuleKind, RuleEvent, RuleOutcome, RuleVariableError, RuleVariables, expand_variables,
    format_variable, valid_variable_name,
};
pub use runtime::{
    AssetLibrary, BUNDLE_FILE, EXPORT_SCENE_FILE, ExportContent, ImportCache, LoadedSkinnedMesh,
    build_road_renderer,
    LodCache,
    MeshGeometry, RuntimeResolver, attach_lod_tiers, base_mesh, executable_dir, load_mesh_geometry,
    load_skinned_mesh, lod_tier_geometry, material_from_imported, open_export, skinned_parts,
    upload_skinned_model,
};
pub use painted::{PaintedGroupData, ScenePaintedGroup};
pub use road::{
    MAX_ROAD_POINTS, MAX_ROAD_WIDTH, ROAD_LIFT, RoadData, RoadSample, RoadShape, SceneRoad,
};
pub use stream_area::{
    MAX_AREA_DISTANCE, PlacedArea, SceneStreamArea, StreamAreaData, StreamPlan, distance_to_area,
    plan_streaming,
};
pub use save::{CURRENT_SAVE_VERSION, SaveGame, SavedEntity, WorldSnapshot};
pub use sprite_atlas::{
    GridRegion, PackedSpriteAtlas, advance_sprite_animations, apply_packed_sprite_uvs,
    next_sprite_animation_frame, pack_sprite_textures, pack_world_sprite_atlas,
};
pub use ui::{
    UiMountItem, authored_ui_text, mount_ui_elements, set_authored_ui_text, sync_ui_variables,
};
pub use world::{point_lights_from_world, spot_lights_from_world};
pub use world::saved_camera_pose;
