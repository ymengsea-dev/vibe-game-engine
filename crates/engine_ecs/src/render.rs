//! Bridges ECS entity data into the renderer: the point where "the
//! renderer draws whatever's in the `World`" replaces "the renderer draws
//! whatever the game code manually kept a reference to".

use bevy_ecs::world::World;
use engine_renderer::{
    CameraBinding, DebugLinePipeline, DebugLineVertex, Drawable, GpuContext, HdrTarget,
    LightsBinding, Mesh, ModelUniform, Pipeline, RendererError, ShadowMap, ShadowPipeline,
    SkyboxBinding, SkyboxPipeline, SpriteAtlasBinding, SpriteBatch, SpriteInstance, SpritePipeline,
    TonemapBinding, TonemapPipeline,
};

use crate::components::{GlobalTransform, MeshRenderer, Sprite};
use crate::propagate::propagate_transforms;

/// Propagates the entity hierarchy's transforms (see
/// [`propagate_transforms`]), then queries the world for every
/// `(GlobalTransform, MeshRenderer)` entity, syncs each one's model-matrix
/// binding to its resolved world-space transform, and renders all of them
/// in a single frame.
///
/// This is a plain function, not a `bevy_ecs` system — it needs `&World`
/// plus the GPU handles (`gpu`/`pipeline`/`shadow_pipeline`/`shadow_map`/
/// `skybox_pipeline`/`skybox`/`hdr_target`/`tonemap_pipeline`/
/// `tonemap_binding`/`camera`/`lights`/`debug_lines`) that a caller (a
/// windowing/app loop) already owns, and there's exactly one call site
/// per frame, so there's no need to route it through `bevy_ecs`'s
/// system/`Res<T>` machinery.
///
/// No camera or light entity lookup yet: `camera`/`lights` are supplied
/// directly by the caller. [`crate::components::Camera`] exists as a
/// component but this function doesn't consume it — wiring "the active
/// camera is whichever entity has a `Camera` component" (and a matching
/// ECS light component) is future work once there's a reason to support
/// more than a fixed scene-wide set.
///
/// # Errors
///
/// Propagates [`RendererError`] from [`GpuContext::render_scene`].
#[allow(
    clippy::too_many_arguments,
    reason = "forwards render_scene's GPU handles as-is; see its own allow for why"
)]
pub fn extract_and_render(
    world: &mut World,
    gpu: &GpuContext,
    pipeline: &Pipeline,
    shadow_pipeline: &ShadowPipeline,
    shadow_map: &ShadowMap,
    skybox_pipeline: &SkyboxPipeline,
    skybox: &SkyboxBinding,
    hdr_target: &HdrTarget,
    tonemap_pipeline: &TonemapPipeline,
    tonemap_binding: &TonemapBinding,
    camera: &CameraBinding,
    lights: &LightsBinding,
    debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
) -> Result<(), RendererError> {
    propagate_transforms(world);

    let mut query = world.query::<(&GlobalTransform, &MeshRenderer)>();

    let mut drawables = Vec::new();
    for (transform, mesh_renderer) in query.iter(&*world) {
        gpu.write_uniform_buffer(
            &mesh_renderer.model.buffer,
            &ModelUniform::from(&transform.0),
        );
        drawables.push(Drawable {
            model: &mesh_renderer.model,
            material: &mesh_renderer.material,
            mesh: &mesh_renderer.mesh,
        });
    }

    gpu.render_scene(
        pipeline,
        shadow_pipeline,
        shadow_map,
        skybox_pipeline,
        skybox,
        hdr_target,
        tonemap_pipeline,
        tonemap_binding,
        camera,
        lights,
        &drawables,
        debug_lines,
    )
}

/// Propagates the entity hierarchy's transforms (see
/// [`propagate_transforms`]), then queries the world for every
/// `(GlobalTransform, Sprite)` entity, builds one [`SpriteBatch`] from all
/// of them, and renders it in a single frame.
///
/// The 2D counterpart to [`extract_and_render`] — see [`Sprite`]'s docs
/// for why it needs no per-entity GPU-binding sync the way `MeshRenderer`
/// does. `pipeline`/`quad`/`camera`/`atlas` are supplied by the caller,
/// same as `extract_and_render`'s GPU handles: one shared sprite pipeline,
/// one static quad mesh, one camera, and one atlas for the whole frame,
/// not per-entity data.
///
/// A sprite entity's world-space rotation comes from
/// [`crate::components::Transform`]'s quaternion via
/// [`glam::Quat::to_scaled_axis`]'s `z` component — exact for a pure
/// rotation about `Z` (the only kind a 2D game produces; anything tilting
/// out of the XY plane isn't representable by a flat sprite's single
/// rotation angle anyway).
///
/// # Errors
///
/// Propagates [`RendererError`] from [`GpuContext::render_sprites`].
pub fn extract_and_render_sprites(
    world: &mut World,
    gpu: &GpuContext,
    sprite_pipeline: &SpritePipeline,
    quad: &Mesh,
    camera: &CameraBinding,
    atlas: &SpriteAtlasBinding,
) -> Result<(), RendererError> {
    propagate_transforms(world);

    let mut batch = SpriteBatch::new();
    let mut query = world.query::<(&GlobalTransform, &Sprite)>();
    for (transform, sprite) in query.iter(&*world) {
        let rotation = transform.0.rotation.to_scaled_axis().z;
        batch.push(SpriteInstance {
            position: transform.0.translation.to_array(),
            size: sprite.size.to_array(),
            rotation,
            uv_min: sprite.uv.min,
            uv_max: sprite.uv.max,
            color: sprite.color,
        });
    }

    gpu.render_sprites(sprite_pipeline, quad, camera, atlas, &batch)
}
