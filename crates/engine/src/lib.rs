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
pub use engine_editor as editor;
pub use engine_network as network;
pub use engine_physics as physics;
pub use engine_platform as platform;
pub use engine_renderer as renderer;
pub use engine_scene as scene;
pub use engine_scripting as scripting;
pub use engine_ui as ui;
pub use engine_utils as utils;

pub use engine_ai as ai;

/// Commonly used engine types, re-exported for convenient glob imports.
///
/// Populated as subsystems are implemented milestone by milestone.
pub mod prelude {
    pub use engine_animation::{
        AnimationState, BlendMotion, StateMachine, StateMachineError, Transition,
        TransitionCondition, blend_poses, compute_skinning_matrices, sample_blend_tree_1d,
        sample_looping, sample_pose,
    };
    pub use engine_asset::{
        AssetDatabase, AssetError, AssetId, AssetLoader, AssetWatcher, ImportedAnimation,
        ImportedAnimationChannels, ImportedAudio, ImportedCamera, ImportedGltf, ImportedImage,
        ImportedInterpolation, ImportedJoint, ImportedKeyframes, ImportedMaterial, ImportedMesh,
        ImportedSkeleton, ImportedSkinWeights, ImportedTexture, LoadHandle, LoadStatus,
        generate_mip_chain, import_gltf_slice, import_texture_bytes, import_wav_bytes,
    };
    pub use engine_audio::{
        AudioContext, AudioError, Bus, ListenerHandle, SpatialTrackHandle, StaticSound,
        StreamingSound, Tween,
    };
    pub use engine_core::logging;
    pub use engine_core::{App, AppState, EngineConfig, EngineError};
    pub use engine_ecs::{Ecs, extract_and_render, extract_and_render_sprites, sync_rigid_bodies};
    pub use engine_editor::{
        AssetEntry, AssetKind, ConsoleLayer, ConsoleLine, ConsoleLog, EditorError, EditorShell,
        EditorState, ScreenDescriptor, Viewport, scan_assets,
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
    pub use engine_renderer::{
        AtlasLayout, Camera, CameraBinding, CameraUniform, DEFAULT_EXPOSURE, DebugLinePipeline,
        DebugLineVertex, DirectionalLight, GpuContext, HdrTarget, LightSet, LightsBinding,
        Material, MaterialBinding, MaterialUniform, Mesh, Pipeline, PixelRect, PointLight,
        Projection, RendererError, ShadowMap, ShadowPipeline, SkyboxBinding, SkyboxPipeline,
        SpriteAtlasBinding, SpriteBatch, SpriteInstance, SpritePipeline, Texture, TextureAtlas,
        TonemapBinding, TonemapPipeline, UvRect, Vertex, cube, decode_rgba8,
        directional_light_view_projection, quad, skybox_uniform,
    };
    pub use engine_scene::{
        AssetRef, CURRENT_SCENE_VERSION, CameraData, MeshRendererData, Prefab, ProjectionData,
        Scene, SceneEntity, SceneError, SpriteData, TransformData,
    };
    pub use engine_utils::Transform;
    pub use glam;
    pub use tracing;
}
