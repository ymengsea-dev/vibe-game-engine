//! # engine
//!
//! Facade crate for the Vibe Game Engine (VGE).
//!
//! Game projects depend on this single crate and access every subsystem
//! through it, so internal crate boundaries can evolve without breaking
//! games:
//!
//! ```
//! use engine::prelude::*;
//! ```

pub use engine_animation as animation;
pub use engine_asset as asset;
pub use engine_audio as audio;
pub use engine_core as core;
pub use engine_ecs as ecs;
#[cfg(feature = "studio")]
pub use engine_editor as editor;
pub use engine_network as network;
pub use engine_physics as physics;
pub use engine_platform as platform;
pub use engine_project as project;
pub use engine_renderer as renderer;
pub use engine_scene as scene;
pub use engine_scripting as scripting;
pub use engine_ui as ui;
pub use engine_utils as utils;

#[cfg(feature = "studio")]
pub use engine_ai as ai;

pub mod app;

/// Commonly used engine types, re-exported for convenient glob imports.
///
/// Populated as subsystems are implemented milestone by milestone.
pub mod prelude {
    pub use crate::app::{Game, GameConfig, GameContext, GameError, Time, run_game};
    #[cfg(feature = "studio")]
    pub use engine_ai::{
        GridCoord, NavError, NavGrid, find_path, find_path_world, follow_path, line_of_sight,
        smooth_path,
    };
    pub use engine_animation::{
        AnimationState, BlendMotion, StateMachine, StateMachineError, Transition,
        TransitionCondition, blend_poses, compute_skinning_matrices, sample_blend_tree_1d,
        sample_looping, sample_pose,
    };
    pub use engine_asset::{
        AssetDatabase, AssetError, AssetId, AssetLoader, AssetWatcher, Bundle as AssetBundle,
        BundleStats, ImportedAnimation, ImportedAnimationChannels, ImportedAudio, ImportedCamera,
        ImportedGltf, ImportedImage, ImportedInterpolation, ImportedJoint, ImportedKeyframes,
        ImportedMaterial, ImportedMesh, ImportedSkeleton, ImportedSkinWeights, ImportedTexture,
        LoadHandle, LoadStatus, generate_mip_chain, import_gltf_slice, import_texture_bytes,
        import_wav_bytes, pack_dir,
    };
    pub use engine_audio::{
        AudioContext, AudioError, Bus, ListenerHandle, SpatialTrackHandle, StaticSound,
        StreamingSound, Tween,
    };
    pub use engine_core::logging;
    pub use engine_core::{App, AppState, EngineConfig, EngineError, FixedTimestep};
    pub use engine_ecs::prelude::{Bundle, ChildOf, Children, Entity, World};
    pub use engine_ecs::{
        BlendMode, Ecs, ParticleEmitter, ParticleEmitterConfig, RenderStats, Rng,
        despawn_instanced_mesh_renderer, despawn_mesh_renderer, despawn_skinned_mesh_renderer,
        despawn_vegetation_renderer, extract_and_render, extract_and_render_sprites,
        extract_particles, sync_rigid_bodies, update_particles,
    };
    #[cfg(feature = "studio")]
    pub use engine_editor::{
        AssetEntry, AssetImporter, AssetIndex, AssetKind, AudioSummary, BottomTab, BuildConfig,
        ConsoleLayer, ConsoleLine, ConsoleLog, DirtyState, EditorDimension, EditorError,
        EditorSession, EditorShell, EditorState, ImportOutcome, ImportRecord, ImportStats,
        ImportedAsset, InspectorTab, MeshSummary, PREFAB_DIR, PanelVisibility, PendingAction,
        PreviewCache, SESSION_FILE, SESSION_VERSION, ScreenDescriptor, TextureSummary,
        TransformSpace, Viewport, Workspace, audio_summary, human_bytes, mesh_summary,
        resolve_asset_ids, scan_assets, texture_summary, waveform_bins, write_prefab,
    };
    pub use engine_physics::{
        Aabb2d, CharacterController, CharacterMovement, CharacterMovement2D, Circle2d, Collider2d,
        ColliderBuilder, ColliderHandle, DebugLine, DebugRenderMode, DebugRenderStyle,
        PhysicsError, PhysicsWorld, RayHit, RigidBodyBuilder, RigidBodyHandle, aabb_vs_aabb,
        aabb_vs_circle, cast_ray, circle_vs_circle, debug_render_lines, move_character,
        move_character_2d,
    };
    pub use engine_platform::{
        ActionMap, Binding, InputState, KeyCode, MouseButton, PlatformError, PlatformEvent,
        PlatformHandler, Window, WindowConfig, WindowEvent, run_windowed,
    };
    pub use engine_project::{CURRENT_PROJECT_VERSION, Project, ProjectError, ProjectManifest};
    pub use engine_renderer::{
        Aabb, AtlasLayout, BloomSettings, Brush, BrushFalloff, Camera, CameraBinding, CameraRig,
        CameraUniform, ColorGrade, DEFAULT_EXPOSURE, DebugLinePipeline, DebugLineVertex,
        DirectionalLight, Frustum, GpuContext, HdrTarget, Heightmap, InstanceBuffer, InstanceRaw,
        InstancedDrawable, InstancedPipeline, JointMatricesUniform, LightSet, LightsBinding,
        MAX_JOINTS, Material, MaterialBinding, MaterialUniform, Mesh, ModelUniform,
        OutlineSettings, ParticleCameraBinding, ParticleCameraUniform, ParticleFrame,
        ParticleInstance, ParticleInstanceBuffer, ParticlePipeline, ParticlePipelines, Pipeline,
        PixelRect, Plane, PointLight, PostProcessStack, PostSettings, Projection, RenderAssets,
        RendererError, ScatterArea, ScatterConfig, ScatterError, ShadowMap, ShadowPipeline,
        SkinnedBinding, SkinnedDrawable, SkinnedMesh, SkinnedPipeline, SkinnedVertex,
        SkyboxBinding, SkyboxPipeline, SpriteAtlasBinding, SpriteBatch, SpriteInstance,
        SpritePipeline, Texture, TextureAtlas, UvRect, VegetationPipeline, Vertex, Wind,
        WindBinding, WindUniform, cube, decode_rgba8, directional_light_view_projection, quad,
        scatter, skybox_uniform,
    };
    pub use engine_scene::{
        AssetRef, CURRENT_SCENE_VERSION, CameraData, MeshRendererData, Prefab, ProjectionData,
        Scene, SceneEntity, SceneError, SpriteData, TransformData,
    };
    pub use engine_ui::{
        Anchor, Color as UiColor, DrawCommand, DrawKind, Node as UiNode, NodeId, PointerInput,
        Rect as UiRect, Style as UiStyle, Ui, Widget as UiWidget,
    };
    pub use engine_utils::{AssetHandle, AssetStore, JobError, JobSystem, Transform};
    pub use glam;
    pub use tracing;
}
