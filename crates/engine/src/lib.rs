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

pub use engine_ai as ai;
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

pub mod app;

/// Commonly used engine types, re-exported for convenient glob imports.
///
/// Populated as subsystems are implemented milestone by milestone.
pub mod prelude {
    pub use crate::app::{
        Game, GameConfig, GameContext, GameError, MAX_FRAME_SECONDS, SavePolicy, SceneParts, Time,
        run_game,
    };
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
        AnimationEvent, AnimationEvents, AnimationPlayer, AudioEmitter, AudioListener, BlendMode,
        Ecs, ParticleEmitter, ParticleEmitterConfig, RenderStats, Rng, SpriteTarget,
        despawn_instanced_mesh_renderer, despawn_mesh_renderer, despawn_skinned_mesh_renderer,
        despawn_vegetation_renderer, extract_and_render, extract_particles, extract_sprites,
        sync_rigid_bodies, update_audio, update_particles,
    };
    #[cfg(feature = "studio")]
    pub use engine_editor::{
        AssetEntry, AssetImporter, AssetIndex, AssetKind, AudioSummary, BottomTab, ConsoleLayer,
        ConsoleLine, ConsoleLog, DirtyState, EditorDimension, EditorError, EditorSession,
        EditorShell, EditorState, ImportOutcome, ImportRecord, ImportStats, ImportedAsset,
        InspectorTab, MeshSummary, PREFAB_DIR, PanelVisibility, PendingAction, PreviewCache,
        ProjectRequest, RecentProjects, SESSION_FILE, SESSION_VERSION, ScreenDescriptor,
        TextureSummary, TransformSpace, Viewport, Workspace, audio_summary, human_bytes,
        mesh_summary, resolve_asset_ids, scan_assets, texture_summary, waveform_bins, write_prefab,
    };
    pub use engine_physics::{
        Aabb2d, CharacterController, CharacterMovement, CharacterMovement2D, Circle2d, Collider2d,
        ColliderBuilder, ColliderHandle, DebugLine, DebugRenderMode, DebugRenderStyle,
        PhysicsError, PhysicsWorld, RayHit, RigidBodyBuilder, RigidBodyHandle, aabb_vs_aabb,
        aabb_vs_circle, cast_ray, circle_vs_circle, debug_render_lines, move_character,
        move_character_2d,
    };
    pub use engine_platform::{
        ActionMap, AxisBinding, Binding, DEFAULT_DEADZONE, GamepadAxis, GamepadButton, Gamepads,
        InputState, KeyCode, MouseButton, PlatformError, PlatformEvent, PlatformHandler, Stick,
        Window, WindowConfig, WindowEvent, run_windowed,
    };
    pub use engine_project::{CURRENT_PROJECT_VERSION, Project, ProjectError, ProjectManifest};
    pub use engine_renderer::{
        Aabb, AlphaMode, AmbientLight, AtlasLayout, BloomSettings, Brush, BrushFalloff, Camera,
        CameraBinding, CameraRig, CameraUniform, ColorGrade, DEFAULT_EXPOSURE, DebugLinePipeline,
        DebugLineVertex, DirectionalLight, Frustum, GpuContext, HdrTarget, Heightmap,
        InstanceBuffer, InstanceRaw, InstancedDrawable, InstancedPipeline, JointMatricesUniform,
        LightSet, LightsBinding, MAX_JOINTS, Material, MaterialBinding, MaterialUniform, Mesh,
        ModelUniform, OutlineSettings, ParticleCameraBinding, ParticleCameraUniform, ParticleFrame,
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
        AssetLibrary, AssetRef, AudioEmitterData, BUNDLE_FILE, BodyKind, CURRENT_SCENE_VERSION,
        CameraData, ColliderData, ColliderShape, EXPORT_SCENE_FILE, ExportContent,
        InstantiateReport, LoadedSkinnedMesh, MeshRendererData, NavGridData, NullResolver, Prefab,
        ProjectionData, RuntimeResolver, SaveGame, SavedEntity, Scene, SceneAudioEmitter,
        SceneCollider, SceneEntity, SceneError, SceneNavGrid, SceneResolver, SpriteData,
        TransformData, WorldSnapshot, capture_renderables, executable_dir, load_mesh_geometry,
        load_skinned_mesh, open_export,
    };
    pub use engine_ui::{
        Anchor, Color as UiColor, DrawCommand, DrawKind, FocusDirection, FocusRing, Node as UiNode,
        NodeId, PointerInput, Rect as UiRect, Style as UiStyle, Ui, Widget as UiWidget,
    };
    pub use engine_utils::{AssetHandle, AssetStore, JobError, JobSystem, Transform};
    pub use glam;
    pub use tracing;
}

#[cfg(test)]
mod tests {
    /// Navigation must be reachable from a runtime build. It used to sit
    /// behind the `studio` feature, which `apps/player` turns off — a
    /// shipped game could not pathfind (T-10). Written against the
    /// prelude, and run under `--no-default-features` as well, so the
    /// gating cannot regress silently.
    #[test]
    fn navigation_is_available_without_the_studio_feature() {
        use crate::prelude::{GridCoord, NavGrid, find_path};

        let grid = NavGrid::new(4, 4, 1.0, glam::Vec2::ZERO).expect("a 4x4 grid is valid");
        let path = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(3, 3))
            .expect("an empty grid always has a path");
        assert_eq!(path.first().copied(), Some(GridCoord::new(0, 0)));
        assert_eq!(path.last().copied(), Some(GridCoord::new(3, 3)));
    }

    /// A nav grid authored in a scene is the one the pathfinder walks.
    ///
    /// This is the seam T-28 exists to close: `engine_ai` knows nothing
    /// about scenes and `engine_scene` knows nothing about pathfinding,
    /// so the only place the two meet is here, in the facade both a game
    /// and the editor use.
    #[test]
    fn a_scene_nav_grid_is_what_the_pathfinder_walks() {
        use crate::prelude::{
            GridCoord, NavGrid, NavGridData, Scene, SceneNavGrid, World, find_path,
        };

        // A 5x3 grid with a wall down the middle column, one gap at the
        // far edge — a path exists but has to go around.
        let mut data = NavGridData::new(5, 3, 1.0, [0.0, 0.0]);
        for row in 0..2 {
            data.blocked[row * 5 + 2] = true;
        }
        let scene = Scene {
            version: crate::scene::CURRENT_SCENE_VERSION,
            entities: Vec::new(),
            nav_grid: Some(data),
        };
        scene.validate().expect("a well-formed grid validates");

        // Through the world, as a loaded scene puts it there.
        let mut world = World::new();
        scene.instantiate(&mut world);
        let authored = world
            .get_resource::<SceneNavGrid>()
            .expect("the scene put its grid in the world")
            .0
            .clone();

        let grid = NavGrid::from_cells(
            authored.width,
            authored.height,
            authored.cell_size,
            glam::Vec2::from_array(authored.origin),
            &authored.blocked,
        )
        .expect("authored cells make a valid grid");

        assert!(grid.is_blocked(GridCoord::new(2, 0)), "the wall is there");
        let path = find_path(&grid, GridCoord::new(0, 0), GridCoord::new(4, 0))
            .expect("there is a way around the wall");
        assert!(
            path.iter().any(|step| step.y == 2),
            "the path detours through the gap rather than through the wall: {path:?}"
        );
        assert!(
            !path.iter().any(|step| grid.is_blocked(*step)),
            "no step of the path is a blocked cell"
        );
    }
}
