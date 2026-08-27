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
//! (floating-point, so radiance above `1.0` survives) and a
//! [`TonemapPipeline`] compresses it into the swapchain's displayable
//! range with an ACES filmic curve instead of a hard clip. A
//! [`DebugLinePipeline`] draws pre-colored [`DebugLineVertex`] line
//! segments (e.g. from `engine_physics::debug_render_lines`) on top of
//! everything else — an optional pass in [`GpuContext::render_scene`],
//! not always-on.
//!
//! Stage 2 "Sprite batch renderer": a second, independent 2D draw path —
//! [`SpritePipeline`] instances a [`quad`] per [`SpriteInstance`] in
//! [`SpriteBatch`] (one draw call per batch), alpha-blended, sampling a
//! [`TextureAtlas`] ([`AtlasLayout`] maps named/gridded regions to
//! normalized UVs). Drawn standalone via [`GpuContext::render_sprites`],
//! not through [`GpuContext::render_scene`] — see the `sprite` module docs
//! for why.
//!
//! Stage 3 "Render graph": [`GpuContext::render_scene`]'s
//! shadow/scene/tonemap sequence now runs through a [`RenderGraph`] —
//! passes declared with the resources they read/write, topologically
//! ordered, instead of a hardcoded sequence — so future passes (Stage 5's
//! post-processing) slot in by declaring dependencies rather than another
//! rewrite of that function. See the `render_graph` module docs.

mod camera;
mod config;
mod error;
mod gpu;
mod hdr;
mod light;
mod material;
mod mesh;
mod model;
mod pipeline;
mod render_graph;
mod shadow;
mod skybox;
mod sprite;
mod texture;

pub use camera::{Camera, CameraUniform, Projection};
pub use config::{build_surface_config, choose_surface_format, should_reconfigure};
pub use error::RendererError;
pub use gpu::GpuContext;
pub use hdr::{DEFAULT_EXPOSURE, TonemapUniform};
pub use light::{
    DirectionalLight, DirectionalLightUniform, LightSet, LightsUniform, MAX_DIRECTIONAL_LIGHTS,
    MAX_POINT_LIGHTS, PointLight, PointLightUniform,
};
pub use material::{Material, MaterialUniform};
pub use mesh::{Mesh, Vertex, cube, quad};
pub use model::ModelUniform;
pub use pipeline::{
    CameraBinding, DebugLinePipeline, DebugLineVertex, Drawable, HdrTarget, LightsBinding,
    MaterialBinding, ModelBinding, Pipeline, ShadowMap, ShadowPipeline, SkyboxBinding,
    SkyboxPipeline, TonemapBinding, TonemapPipeline,
};
pub use render_graph::RenderGraph;
pub use shadow::{SHADOW_MAP_SIZE, ShadowUniform, directional_light_view_projection};
pub use skybox::{SkyboxUniform, skybox_uniform};
pub use sprite::{
    AtlasLayout, PixelRect, SpriteAtlasBinding, SpriteBatch, SpriteInstance, SpritePipeline,
    TextureAtlas, UvRect,
};
pub use texture::{Texture, decode_rgba8};
