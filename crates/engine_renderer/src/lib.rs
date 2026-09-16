//! # engine_renderer
//!
//! GPU renderer built on wgpu: device management, pipelines, materials, meshes, lighting, shadows, and post-processing.
//!
//! ## Status
//!
//! Milestones 1-3 complete: [`GpuContext`] (wgpu init, first frame,
//! resize), [`Camera`] view/projection matrices, [`Mesh`] vertex/index
//! buffer management (with built-in [`cube`] test geometry), a
//! [`Pipeline`] that draws camera-transformed geometry
//! ([`GpuContext::render_scene`], multiple [`Drawable`]s per frame, each
//! with its own [`ModelUniform`]/[`ModelBinding`] so per-entity
//! `Transform`s actually move things), and [`Texture`] loading (decode
//! via [`decode_rgba8`], GPU upload, sampling). Milestone 6 in progress:
//! [`Material`] (metallic-roughness workflow factors) is on the GPU via
//! [`MaterialBinding`], [`DirectionalLight`]/[`PointLight`]
//! ([`LightSet`]) are real inputs to a Cook-Torrance GGX BRDF in
//! `pbr.wgsl` — base color, metallic, and roughness now all affect
//! shading — and the first directional light casts real shadows: a
//! [`ShadowPipeline`] renders scene depth from the light's point of view
//! into a [`ShadowMap`], sampled back in the main pass with 3x3 PCF
//! filtering. A [`SkyboxPipeline`] paints a procedural sky (horizon
//! gradient plus sun glow, no cubemap asset needed) behind all geometry.
//! Milestone 6 is complete: the main pass renders into a [`HdrTarget`]
//! (floating-point, so radiance above `1.0` survives) and the
//! post-processing composite pass compresses it into the swapchain's
//! displayable range with an ACES filmic curve instead of a hard clip. A
//! [`DebugLinePipeline`] draws pre-colored [`DebugLineVertex`] line
//! segments (e.g. from `engine_physics::debug_render_lines`) on top of
//! everything else — an optional pass in [`GpuContext::render_scene`],
//! not always-on.
//!
//! Stage 2 "Sprite batch renderer": the 2D draw path — [`SpritePipeline`]
//! instances a [`quad`] per [`SpriteInstance`] in [`SpriteBatch`] (one
//! draw call per batch), alpha-blended, sampling a [`TextureAtlas`]
//! ([`AtlasLayout`] maps named/gridded regions to normalized UVs). Drawn
//! inside [`GpuContext::render_scene`]'s scene pass as a [`SpriteFrame`],
//! against the same HDR target and depth buffer as 3D geometry, so one
//! frame can hold both — see the `sprite` module docs.
//!
//! Stage 3 "Render graph": [`GpuContext::render_scene`]'s
//! shadow/scene/tonemap sequence now runs through a [`RenderGraph`] —
//! passes declared with the resources they read/write, topologically
//! ordered, instead of a hardcoded sequence — so future passes (Stage 5's
//! post-processing) slot in by declaring dependencies rather than another
//! rewrite of that function. See the `render_graph` module docs.
//!
//! Stage 3 "Asset-handle-backed MeshRenderer/materials": [`RenderAssets`]
//! is the GPU-resident store [`Mesh`]/[`MaterialBinding`] live in once a
//! component only holds a handle to one — `engine_ecs::MeshRenderer`
//! resolves its `AssetHandle<Mesh>`/`AssetHandle<MaterialBinding>` against
//! it each frame instead of owning the GPU resource directly, so multiple
//! entities sharing one mesh cost one GPU upload, not one per entity.
//!
//! Stage 3 "Frustum culling": every [`Mesh`] carries its object-space
//! [`Aabb`] (computed once at upload), [`Camera::frustum`] derives a
//! [`Frustum`] of six inward-facing [`Plane`]s from the view-projection
//! matrix, and [`Frustum::intersects_aabb`] is the conservative per-entity
//! visibility test `engine_ecs::extract_and_render` runs to keep
//! off-screen meshes out of the scene draw list. See the `bounds` module.
//!
//! Stage 3 "GPU vertex skinning": [`SkinnedVertex`]/[`SkinnedMesh`] carry
//! per-vertex joint indices/weights; [`SkinnedPipeline`]'s vertex shader
//! (`pbr_common.wgsl` + `skinned_pbr_vs.wgsl`) blends a
//! [`JointMatricesUniform`] of per-joint skinning matrices — supplied by
//! the CPU from `engine_animation::compute_skinning_matrices` — by weight
//! to deform the mesh before the model transform. Drawn by
//! [`GpuContext::render_scene`] alongside unskinned geometry. See the
//! `skinning` module.
//!
//! Stage 3 "MSAA": the main color pass renders at
//! [`GpuContext::msaa_sample_count`] samples (4x where the adapter
//! supports it for the HDR format, else 1) into a multisampled companion
//! of the [`HdrTarget`], resolved into the single-sample texture the
//! tonemap pass reads. Every scene-pass pipeline
//! ([`Pipeline`]/[`SkinnedPipeline`]/[`SkyboxPipeline`]/[`DebugLinePipeline`])
//! is built at that count; shadow and tonemap stay single-sample.
//!
//! Stage 3 "Instancing": [`InstancedPipeline`]'s vertex shader
//! (`pbr_common.wgsl` + `instanced_pbr_vs.wgsl`) reads each copy's model
//! matrix from an [`InstanceRaw`] instance-step vertex buffer instead of a
//! `@group(2)` uniform, so N copies of one [`Mesh`] draw in a single
//! `draw_indexed(.., 0..N)`. [`GpuContext::render_scene`] takes a slice of
//! [`InstancedDrawable`]s alongside the per-object ones. See the
//! `instancing` module.
//!
//! Stage 5 "Post-processing stack": [`PostProcessStack`] (built by
//! [`GpuContext::create_post_process_stack`], configured by
//! [`PostSettings`]) is the `"post_process"` node of
//! [`GpuContext::render_scene`]'s render graph — it replaced the plain
//! tonemap pass. It runs a bright-pass + ping-pong separable-blur bloom
//! chain into half-resolution targets, then one composite pass that adds
//! bloom back, applies a linear-HDR color grade ([`ColorGrade`]:
//! exposure, white balance, contrast, saturation, color filter), draws a
//! screen-space Sobel toon outline ([`OutlineSettings`]), and finishes
//! with the same ACES curve the old tonemap pass used —
//! [`PostSettings::IDENTITY`] reproduces that pass exactly. See the `post`
//! module.
//!
//! Stage 5 "Particle system": [`ParticleInstance`]s (produced by
//! `engine_ecs::particles`' CPU simulation) are drawn as camera-facing
//! billboards last in the scene pass, into the HDR target, so bloom and
//! tonemap treat them as radiance. [`GpuContext::create_particle_pipelines`]
//! builds an alpha-blended and an additive [`ParticlePipeline`] (one
//! `particle.wgsl`, premultiplied output); [`GpuContext::render_scene`]
//! takes an optional [`ParticleFrame`] and uploads/draws the two
//! blend-mode slices. Billboard basis comes from
//! [`ParticleCameraUniform`]; the quads are shaped into soft round blobs
//! in-shader, no texture. See the `particle` module.
//!
//! Stage 5 "Terrain (heightmap) + sculpt": [`Heightmap`] is an editable
//! `resolution x resolution` grid of heights; [`Heightmap::mesh_data`]
//! turns it into `([Vertex], [u32])` for
//! [`GpuContext::create_mesh_dynamic`] (a `COPY_DST` vertex buffer), and
//! [`GpuContext::write_mesh_vertices`] re-uploads it in place after a
//! [`Brush`] sculpt op (`raise_lower` / `smooth` / `flatten`, with
//! [`BrushFalloff`]). [`Heightmap::normal_at`] / [`Heightmap::slope_at`]
//! query the surface. A terrain is then just a [`Mesh`] drawn through the
//! normal `MeshRenderer` path. All heightmap/sculpt math is pure and
//! `wgpu`-free. See the `terrain` module.
//!
//! Stage 5 "Validation Game 4": [`CameraRig`] is a third-person
//! follow/orbit camera — a look-at `target`, orbit `yaw`/`pitch`,
//! `distance`/`height`, and a smoothed `follow_speed`;
//! [`CameraRig::apply`] drives a [`Camera`] each frame. Pure math. See
//! the `camera_rig` module.
//!
//! Stage 5 "Procedural scattering": [`scatter`] fills a [`ScatterArea`]
//! (jittered-grid sampling, seeded, deterministic) with placement
//! `engine_utils::Transform`s for instanced rendering, honouring a
//! [`ScatterConfig`]'s masks — keep-out circles, terrain slope limit,
//! world-Y band, density thinning — plus per-instance yaw, scale, and
//! tilt toward the terrain normal. Pure CPU, samples [`Heightmap`]. See
//! the `scatter` module.
//!
//! Stage 5 "Vegetation rendering": [`VegetationPipeline`]
//! (`pbr_common.wgsl` + `vegetation_vs.wgsl`, built by
//! [`GpuContext::create_vegetation_pipeline`]) is a second instanced PBR
//! pipeline whose vertex stage bends each blade along a global [`Wind`]
//! ([`WindBinding`] at `@group(2)`, pivoting at object-space `y = 0`,
//! per-plant phase from the instance's world origin). It reuses
//! [`InstanceRaw`]/[`InstancedDrawable`] unchanged;
//! [`GpuContext::render_scene`] takes an optional
//! `(&VegetationPipeline, &WindBinding, &[InstancedDrawable])` and draws
//! it in the scene pass. See the `vegetation` module.

mod bounds;
mod camera;
mod camera_rig;
mod config;
mod debug_draw;
mod error;
mod gpu;
mod instancing;
mod light;
mod lod;
mod material;
mod mesh;
mod model;
mod particle;
mod pipeline;
mod post;
mod render_assets;
mod render_graph;
mod scatter;
mod shadow;
mod skinning;
mod skybox;
mod sprite;
mod terrain;
mod text;
mod texture;
mod ui_pass;
mod vegetation;

pub use bounds::{Aabb, Frustum, Plane};
pub use camera::{Camera, CameraUniform, Projection};
pub use camera_rig::CameraRig;
pub use config::{
    build_surface_config, choose_msaa_sample_count, choose_surface_format, should_reconfigure,
};
pub use debug_draw::DebugDraw;
pub use error::RendererError;
pub use gpu::{GpuContext, REQUESTED_MSAA_SAMPLE_COUNT};
pub use instancing::{InstanceBuffer, InstanceRaw, InstancedDrawable, InstancedPipeline};
pub use light::{
    AmbientLight, DirectionalLight, DirectionalLightUniform, LightSet, LightsUniform,
    MAX_DIRECTIONAL_LIGHTS, MAX_POINT_LIGHTS, PointLight, PointLightUniform,
};
pub use lod::{LodLevel, LodSelector, LodSet, StreamQueue};
pub use material::{AlphaMode, Material, MaterialUniform};
pub use mesh::{Mesh, Vertex, cube, quad};
pub use model::ModelUniform;
pub use particle::{
    ParticleCameraBinding, ParticleCameraUniform, ParticleFrame, ParticleInstance,
    ParticleInstanceBuffer, ParticlePipeline, ParticlePipelines,
};
pub use pipeline::{
    CameraBinding, DEPTH_FORMAT, DebugLinePipeline, DebugLineVertex, Drawable, HdrTarget,
    LightsBinding, MaterialBinding, MaterialMaps, ModelBinding, Pipeline, RenderTarget, ShadowMap,
    ShadowPipeline, SkyboxBinding, SkyboxPipeline,
};
pub use post::{
    BloomSettings, BlurUniform, ColorGrade, CompositeUniform, DEFAULT_EXPOSURE, OutlineSettings,
    PostProcessStack, PostSettings, PrefilterUniform,
};
pub use render_assets::RenderAssets;
pub use render_graph::RenderGraph;
pub use scatter::{ScatterArea, ScatterConfig, ScatterError, scatter};
pub use shadow::{SHADOW_MAP_SIZE, ShadowUniform, directional_light_view_projection};
pub use skinning::{
    JointMatricesUniform, MAX_JOINTS, SkinnedBinding, SkinnedDrawable, SkinnedMesh,
    SkinnedPipeline, SkinnedVertex,
};
pub use skybox::{SkyboxUniform, skybox_uniform};
pub use sprite::{
    AtlasLayout, PixelRect, SpriteAtlasBinding, SpriteBatch, SpriteFrame, SpriteInstance,
    SpritePipeline, TextureAtlas, UvRect,
};
pub use terrain::{Brush, BrushFalloff, Heightmap};
pub use text::{
    GLYPH_ADVANCE, GLYPH_HEIGHT, GLYPH_WIDTH, GlyphAtlas, GlyphUv, LINE_ADVANCE, PositionedGlyph,
    TextLayout, covers, glyph_bitmap, layout_text,
};
pub use texture::{Texture, decode_rgba8};
pub use ui_pass::{ScreenUniform, UiPipeline, UiQuad};
pub use vegetation::{VegetationPipeline, Wind, WindBinding, WindUniform};
