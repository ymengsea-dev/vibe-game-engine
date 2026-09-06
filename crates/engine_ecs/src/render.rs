//! Bridges ECS entity data into the renderer: the point where "the
//! renderer draws whatever's in the `World`" replaces "the renderer draws
//! whatever the game code manually kept a reference to".

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use engine_renderer::{
    Aabb, CameraBinding, DebugLinePipeline, DebugLineVertex, Drawable, Frustum, GpuContext,
    HdrTarget, InstanceBuffer, InstanceRaw, InstancedDrawable, InstancedPipeline, LightsBinding,
    Mesh, ModelUniform, ParticleFrame, Pipeline, PostProcessStack, RenderAssets, RenderTarget,
    RendererError, ShadowMap, ShadowPipeline, SkinnedDrawable, SkinnedPipeline, SkyboxBinding,
    SkyboxPipeline, SpriteAtlasBinding, SpriteBatch, SpriteFrame, SpriteInstance, SpritePipeline,
    VegetationPipeline, WindBinding,
};
use glam::Mat4;

use crate::components::{
    GlobalTransform, InstancedMeshRenderer, MeshRenderer, SkinnedMeshRenderer, Sprite,
    VegetationRenderer,
};
use crate::propagate::propagate_transforms;

/// What one [`extract_and_render`] call did this frame.
///
/// `meshes_total - meshes_drawn` is the number of meshes frustum culling
/// removed from the scene pass. Every resolved mesh — culled or not —
/// still renders into the shadow map, so `meshes_total` is also the
/// shadow-caster count.
///
/// Of the `meshes_drawn` visible meshes, `auto_batched_drawn` were folded
/// into `auto_batches` instanced draws (see [`extract_and_render`]'s GPU
/// batching); the rest were one `Drawable` each. Scene-pass mesh draw
/// calls this frame are therefore `(meshes_drawn - auto_batched_drawn) +
/// auto_batches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderStats {
    /// `MeshRenderer` entities with both their `mesh` and `material`
    /// handles resolved against `RenderAssets`.
    pub meshes_total: u32,
    /// Of `meshes_total`, how many were inside the camera frustum and so
    /// drawn in the scene pass — as individual draws plus
    /// `auto_batched_drawn` folded into batches.
    pub meshes_drawn: u32,
    /// `SkinnedMeshRenderer` entities drawn this frame (both handles
    /// resolved). Not frustum-culled — an animated pose can deform a mesh
    /// outside its bind-pose bounds.
    pub skinned_drawn: u32,
    /// Total instances across every `InstancedMeshRenderer` entity, before
    /// culling. Does not include GPU-batched `MeshRenderer`s
    /// (`auto_batched_drawn` covers those).
    pub instances_total: u32,
    /// Of `instances_total`, how many were inside the camera frustum and
    /// so packed into an instance buffer and drawn.
    pub instances_drawn: u32,
    /// How many instanced draws GPU batching synthesized this frame by
    /// merging visible `MeshRenderer`s that share a mesh+material.
    pub auto_batches: u32,
    /// How many visible `MeshRenderer`s were merged into those
    /// `auto_batches` draws. `auto_batched_drawn - auto_batches` is the
    /// scene-pass draw calls batching saved.
    pub auto_batched_drawn: u32,
    /// Total plants across every `VegetationRenderer` entity, before
    /// culling. `0` when [`extract_and_render`] is called without a
    /// vegetation pipeline.
    pub vegetation_total: u32,
    /// Of `vegetation_total`, how many were inside the camera frustum and
    /// so drawn through the wind-animated vegetation pipeline.
    pub vegetation_drawn: u32,
    /// `Sprite` entities drawn this frame. `0` when the caller passed no
    /// sprite pipeline/atlas, and `0` means the sprite draw was skipped
    /// entirely — no instance buffer, no pipeline switch.
    pub sprites_drawn: u32,
}

/// Whether a mesh whose object-space extent is `local_bounds`, positioned
/// by `world_from_local`, falls inside `frustum` and should be drawn.
///
/// The single place camera-frustum culling happens for `MeshRenderer`
/// entities: transform the mesh's bounds into world space
/// ([`Aabb::transformed`]) and run the conservative plane test
/// ([`Frustum::intersects_aabb`]) — which never reports a visible mesh as
/// outside.
fn is_visible(frustum: &Frustum, local_bounds: &Aabb, world_from_local: &Mat4) -> bool {
    frustum.intersects_aabb(&local_bounds.transformed(world_from_local))
}

/// The GPU handles a caller supplies for the sprite draw: the pipeline,
/// the unit quad every instance stamps out, and the atlas they all
/// sample. The instances themselves come from the world, so this is the
/// only part of a [`SpriteFrame`] the caller has to provide.
///
/// `None` (no atlas registered, say) skips sprites for that frame — the
/// entities stay spawned, they simply are not drawn.
#[derive(Clone, Copy)]
pub struct SpriteTarget<'a> {
    /// The sprite pipeline, built with
    /// [`engine_renderer::GpuContext::create_sprite_pipeline`].
    pub pipeline: &'a SpritePipeline,
    /// The unit quad, typically [`engine_renderer::quad`] uploaded once.
    pub quad: &'a Mesh,
    /// The atlas every sprite in the world samples.
    pub atlas: &'a SpriteAtlasBinding,
}

/// How far `point` lies into the view volume, for back-to-front sorting.
///
/// Measured against the frustum's near plane, whose normal points along
/// the view direction — so a larger value is farther from the camera.
/// Using the frustum avoids threading the camera's eye position through
/// an already very wide call.
fn view_depth(frustum: &Frustum, point: glam::Vec3) -> f32 {
    let near = &frustum.planes[4];
    near.normal.dot(point) + near.d
}

/// The frustum-visible subset of `instances`, packed as [`InstanceRaw`]
/// records — the per-instance culling an `InstancedMeshRenderer` runs
/// each frame, factored out so it's testable without a GPU.
fn visible_instances(
    frustum: &Frustum,
    local_bounds: &Aabb,
    instances: &[engine_utils::Transform],
) -> Vec<InstanceRaw> {
    instances
        .iter()
        .filter(|transform| is_visible(frustum, local_bounds, &transform.to_matrix()))
        .map(InstanceRaw::from)
        .collect()
}

/// Minimum group size for GPU batching. A group of visible `MeshRenderer`s
/// smaller than this is drawn one `Drawable` at a time; at this size or
/// larger it's collapsed into a single instanced draw. Two is already a
/// win (two draws + four bind-group sets become one draw + two sets + a
/// small per-frame buffer upload). A configurable threshold is future
/// work.
const MIN_AUTO_BATCH_SIZE: usize = 2;

/// How [`extract_and_render`] should split a frame's visible meshes:
/// `individuals` are drawn one `Drawable` each; each entry of `batches` is
/// a set of indices (into the slice handed to [`plan_auto_batches`]) that
/// share a mesh+material and collapse into one instanced draw.
#[derive(Debug, Default, PartialEq, Eq)]
struct BatchPlan {
    individuals: Vec<usize>,
    batches: Vec<Vec<usize>>,
}

/// Groups `keys` — one `(mesh, material)` identity per visible mesh, in
/// encounter order — into a [`BatchPlan`]. Any identity shared by
/// [`MIN_AUTO_BATCH_SIZE`] or more entries becomes a batch; the rest are
/// individuals. Group order, and index order within each group, both
/// follow first encounter, so the resulting draw list is stable frame to
/// frame.
///
/// O(n) in the number of visible meshes (one hash lookup each), plus O(g)
/// over the distinct groups.
fn plan_auto_batches(keys: &[(engine_utils::AssetId, engine_utils::AssetId)]) -> BatchPlan {
    use std::collections::HashMap;

    let mut group_of: HashMap<(engine_utils::AssetId, engine_utils::AssetId), usize> =
        HashMap::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let slot = *group_of.entry(*key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(index);
    }

    let mut plan = BatchPlan::default();
    for group in groups {
        if group.len() >= MIN_AUTO_BATCH_SIZE {
            plan.batches.push(group);
        } else {
            plan.individuals.extend(group);
        }
    }
    plan
}

/// One frustum-visible `MeshRenderer`, captured with its asset-handle
/// identities so [`plan_auto_batches`] can group the ones that share a
/// mesh+material before any GPU work.
struct VisibleMesh<'a> {
    mesh_handle: engine_utils::AssetHandle<Mesh>,
    material_handle: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    mesh: &'a Mesh,
    material: &'a engine_renderer::MaterialBinding,
    model: &'a engine_renderer::ModelBinding,
    world: Mat4,
}

/// Propagates the entity hierarchy's transforms (see
/// [`propagate_transforms`]), then queries the world for every
/// `(GlobalTransform, MeshRenderer)` entity, syncs each one's model-matrix
/// binding to its resolved world-space transform, culls the ones outside
/// `frustum` from the scene draw list, and renders the frame.
///
/// Culling is camera-only: a mesh outside `frustum` is dropped from the
/// scene pass but still drawn into the shadow map, so its shadow can fall
/// into view even when the mesh itself cannot. Light-frustum culling of
/// shadow casters is future work. The returned [`RenderStats`] reports the
/// total and drawn mesh counts.
///
/// **GPU batching:** among the frustum-visible `MeshRenderer`s, any group
/// that shares both a `mesh` and a `material` handle (2+ entities) is
/// folded into a single instanced draw via the same path
/// `InstancedMeshRenderer` uses — an internal planner does the grouping,
/// then each group's world matrices are packed into a per-frame instance
/// buffer. The rest are drawn one `Drawable` each. This cuts scene-pass
/// draw calls and bind-group churn for repeated props with no API change;
/// [`RenderStats::auto_batches`]/[`RenderStats::auto_batched_drawn`]
/// report it. The shadow pass is unaffected — every mesh is still drawn
/// individually into the shadow map (no instanced shadow shader yet).
///
/// `(GlobalTransform, SkinnedMeshRenderer)` entities are also drawn, with
/// `skinned_pipeline`, after the unskinned geometry. Their model matrix is
/// synced here from `GlobalTransform`; their joint matrices must already
/// have been uploaded by the caller into `skin.joints_buffer` (this crate
/// doesn't depend on `engine_animation`). Skinned meshes are not
/// frustum-culled and cast no shadows yet.
///
/// `InstancedMeshRenderer` entities are drawn last, with
/// `instanced_pipeline` — one `draw_indexed(.., 0..N)` per entity. Each
/// instance is frustum-culled individually and the visible ones packed
/// into a per-frame instance buffer; [`RenderStats`] reports the
/// total/drawn instance counts. Instanced meshes cast no shadows yet.
///
/// **Transparent surfaces** (materials whose `AlphaMode` is `Blend`)
/// sit out the opaque path entirely: they are never GPU-batched
/// (batching reorders, and blending is order-dependent) and are sorted
/// back-to-front by view depth before being handed to the transparent
/// pass, which tests depth without writing it.
///
/// `sprites` (`Some(SpriteTarget)`) turns on `Sprite` processing: every
/// sprite entity is collected by [`extract_sprites`] and drawn inside
/// the scene pass, against the same HDR target and depth buffer as the
/// 3D geometry, so a frame can hold both and post-processing covers
/// them equally. `None` — or a world with no sprites — skips the draw
/// entirely and leaves [`RenderStats::sprites_drawn`] at `0`.
///
/// `particles` (an `engine_renderer::ParticleFrame`, e.g. from
/// [`crate::particles::extract_particles`]) is forwarded straight to
/// `render_scene`, which draws the billboards last in the scene pass, into
/// the HDR target so post-processing bloom/tonemap apply. `None` draws no
/// particles; particles are neither culled here nor cast shadows.
///
/// `vegetation` (`Some((&VegetationPipeline, &WindBinding))`) turns on
/// [`crate::components::VegetationRenderer`] processing: each entity's
/// plants are per-instance frustum-culled and packed into a per-frame
/// instance buffer exactly like `InstancedMeshRenderer`, then drawn
/// through the wind-animated pipeline. `None` skips vegetation entirely
/// (and leaves `RenderStats::vegetation_total` at `0`). Vegetation casts
/// no shadows yet.
///
/// `ui` (`Some((&UiPipeline, &[UiQuad]))`) is drawn last of all, after
/// post-processing has composited, straight onto the target with no
/// depth — a HUD must not be tonemapped or occluded.
///
/// This is a plain function, not a `bevy_ecs` system — it needs `&World`
/// plus the GPU handles (`gpu`/`pipeline`/`shadow_pipeline`/`shadow_map`/
/// `skybox_pipeline`/`skybox`/`hdr_target`/`post`/`camera`/`lights`/
/// `debug_lines`) that a caller (a
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
/// A [`MeshRenderer`] whose `mesh` or `material` handle isn't (or is no
/// longer) registered in `assets` is skipped — logged once via
/// `tracing::warn!`, not treated as a hard error — so one entity with a
/// stale handle doesn't take down the whole frame.
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
    assets: &RenderAssets,
    gpu: &GpuContext,
    pipeline: &Pipeline,
    shadow_pipeline: &ShadowPipeline,
    shadow_map: &ShadowMap,
    skybox_pipeline: &SkyboxPipeline,
    skybox: &SkyboxBinding,
    hdr_target: &HdrTarget,
    post: &PostProcessStack,
    camera: &CameraBinding,
    frustum: &Frustum,
    lights: &LightsBinding,
    skinned_pipeline: &SkinnedPipeline,
    instanced_pipeline: &InstancedPipeline,
    debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
    sprites: Option<SpriteTarget<'_>>,
    particles: Option<ParticleFrame<'_>>,
    vegetation: Option<(&VegetationPipeline, &WindBinding)>,
    ui: Option<(&engine_renderer::UiPipeline, &[engine_renderer::UiQuad])>,
) -> Result<RenderStats, RendererError> {
    extract_and_render_to(
        None,
        world,
        assets,
        gpu,
        pipeline,
        shadow_pipeline,
        shadow_map,
        skybox_pipeline,
        skybox,
        hdr_target,
        post,
        camera,
        frustum,
        lights,
        skinned_pipeline,
        instanced_pipeline,
        debug_lines,
        sprites,
        particles,
        vegetation,
        ui,
    )
}

/// [`extract_and_render`], but compositing into `target` when one is
/// given instead of the swapchain.
///
/// `None` is exactly [`extract_and_render`] — the window path. `Some` is
/// for callers drawing somewhere else: the editor's Scene view renders
/// into an off-screen texture it then shows inside an egui panel, using
/// the same extraction and the same passes a shipped game uses, so what
/// the editor displays is what the game will draw.
///
/// A `target`'s format must match the one `post` was built for; see
/// [`engine_renderer::GpuContext::create_post_process_stack_for_format`].
///
/// # Errors
///
/// Propagates [`RendererError`] from the underlying render call.
#[allow(
    clippy::too_many_arguments,
    reason = "forwards render_scene's GPU handles as-is; see its own allow for why"
)]
pub fn extract_and_render_to(
    target: Option<RenderTarget<'_>>,
    world: &mut World,
    assets: &RenderAssets,
    gpu: &GpuContext,
    pipeline: &Pipeline,
    shadow_pipeline: &ShadowPipeline,
    shadow_map: &ShadowMap,
    skybox_pipeline: &SkyboxPipeline,
    skybox: &SkyboxBinding,
    hdr_target: &HdrTarget,
    post: &PostProcessStack,
    camera: &CameraBinding,
    frustum: &Frustum,
    lights: &LightsBinding,
    skinned_pipeline: &SkinnedPipeline,
    instanced_pipeline: &InstancedPipeline,
    debug_lines: Option<(&DebugLinePipeline, &[DebugLineVertex])>,
    sprites: Option<SpriteTarget<'_>>,
    particles: Option<ParticleFrame<'_>>,
    vegetation: Option<(&VegetationPipeline, &WindBinding)>,
    ui: Option<(&engine_renderer::UiPipeline, &[engine_renderer::UiQuad])>,
) -> Result<RenderStats, RendererError> {
    propagate_transforms(world);

    // Sprites first: `extract_sprites` needs `&mut World`, and every
    // collection below borrows the world shared for the rest of the
    // call. The batch it returns owns its instances, so nothing here
    // keeps that mutable borrow alive.
    let sprite_batch = sprites.map(|_| extract_sprites(world, frustum));

    // All query states built up front: `world.query` needs `&mut World`,
    // but the collected drawables below borrow `&World` shared, so a
    // later `world.query` would conflict.
    let mut mesh_query = world.query::<(&GlobalTransform, &MeshRenderer)>();
    let mut skinned_query = world.query::<(&GlobalTransform, &SkinnedMeshRenderer)>();
    let mut instanced_query = world.query::<&InstancedMeshRenderer>();
    let mut vegetation_query = world.query::<&VegetationRenderer>();

    // Every resolved mesh is a shadow caster; `visible` is the subset
    // inside `frustum` that the scene pass draws — either individually or,
    // where several share a mesh+material, folded into one instanced draw
    // (GPU batching, planned below).
    let mut shadow_casters = Vec::new();
    // `(view depth, drawable)` so the sort key survives the borrow.
    let mut transparent: Vec<(f32, Drawable<'_>)> = Vec::new();
    let mut visible: Vec<VisibleMesh<'_>> = Vec::new();
    for (transform, mesh_renderer) in mesh_query.iter(&*world) {
        let Some(mesh) = assets.meshes.get(mesh_renderer.mesh) else {
            tracing::warn!(
                mesh = ?mesh_renderer.mesh,
                "skipping MeshRenderer: mesh handle not registered in RenderAssets"
            );
            continue;
        };
        let Some(material) = assets.materials.get(mesh_renderer.material) else {
            tracing::warn!(
                material = ?mesh_renderer.material,
                "skipping MeshRenderer: material handle not registered in RenderAssets"
            );
            continue;
        };

        // Built once, then used for both the model uniform and the cull
        // test (which needs it to place the mesh's local bounds in world
        // space) rather than recomputing it in each.
        let world_from_local = transform.0.to_matrix();

        // Written for every caster (not just the visible ones) so a
        // culled mesh still casts its shadow with an up-to-date matrix.
        gpu.write_uniform_buffer(
            &mesh_renderer.model.buffer,
            &ModelUniform {
                model: world_from_local.to_cols_array_2d(),
            },
        );

        shadow_casters.push(Drawable {
            model: &mesh_renderer.model,
            material,
            mesh,
        });

        if is_visible(frustum, &mesh.local_bounds, &world_from_local) {
            // Transparent surfaces sit out the opaque path entirely:
            // they must not be batched (batching reorders) and they need
            // their own back-to-front sort below.
            if material.transparent {
                transparent.push((
                    view_depth(frustum, world_from_local.w_axis.truncate()),
                    Drawable {
                        model: &mesh_renderer.model,
                        material,
                        mesh,
                    },
                ));
                continue;
            }
            visible.push(VisibleMesh {
                mesh_handle: mesh_renderer.mesh,
                material_handle: mesh_renderer.material,
                mesh,
                material,
                model: &mesh_renderer.model,
                world: world_from_local,
            });
        }
    }

    // Sorted back-to-front, so nearer surfaces blend over farther ones.
    // `total_cmp`, not `partial_cmp`: a NaN depth must not panic a frame.
    transparent.sort_by(|a, b| b.0.total_cmp(&a.0));
    let transparent: Vec<Drawable<'_>> = transparent.into_iter().map(|(_, d)| d).collect();

    // Skinned meshes: model matrix synced here like the unskinned path;
    // joint matrices are the caller's to upload into `skin.joints_buffer`
    // before this call. Not frustum-culled (bind-pose bounds can't bound
    // an arbitrary pose) and not shadow casters (no skinned shadow shader).
    let mut skinned_drawables = Vec::new();
    for (transform, skinned) in skinned_query.iter(&*world) {
        let Some(mesh) = assets.skinned_meshes.get(skinned.mesh) else {
            tracing::warn!(
                mesh = ?skinned.mesh,
                "skipping SkinnedMeshRenderer: skinned mesh handle not registered in RenderAssets"
            );
            continue;
        };
        let Some(material) = assets.materials.get(skinned.material) else {
            tracing::warn!(
                material = ?skinned.material,
                "skipping SkinnedMeshRenderer: material handle not registered in RenderAssets"
            );
            continue;
        };

        gpu.write_uniform_buffer(
            &skinned.skin.model_buffer,
            &ModelUniform {
                model: transform.0.to_matrix().to_cols_array_2d(),
            },
        );

        skinned_drawables.push(SkinnedDrawable {
            skin: &skinned.skin,
            material,
            mesh,
        });
    }

    // Instanced meshes: one draw call per entity. Each instance is
    // frustum-culled individually against the mesh's bounds; the visible
    // ones are packed into a per-frame instance buffer. Not shadow casters
    // (no instanced shadow shader).
    struct PendingInstanced<'a> {
        material: &'a engine_renderer::MaterialBinding,
        mesh: &'a Mesh,
        raws: Vec<InstanceRaw>,
    }
    let mut instances_total = 0u32;
    let mut pending_instanced: Vec<PendingInstanced<'_>> = Vec::new();
    for instanced in instanced_query.iter(&*world) {
        let Some(mesh) = assets.meshes.get(instanced.mesh) else {
            tracing::warn!(
                mesh = ?instanced.mesh,
                "skipping InstancedMeshRenderer: mesh handle not registered in RenderAssets"
            );
            continue;
        };
        let Some(material) = assets.materials.get(instanced.material) else {
            tracing::warn!(
                material = ?instanced.material,
                "skipping InstancedMeshRenderer: material handle not registered in RenderAssets"
            );
            continue;
        };

        instances_total += instanced.instances.len() as u32;
        let raws = visible_instances(frustum, &mesh.local_bounds, &instanced.instances);

        if !raws.is_empty() {
            pending_instanced.push(PendingInstanced {
                material,
                mesh,
                raws,
            });
        }
    }

    // GPU batching: fold groups of visible `MeshRenderer`s that share a
    // mesh+material into instanced draws, through the exact path the
    // explicit `InstancedMeshRenderer`s above use. Every one of them is
    // still in `shadow_casters`, drawn individually into the shadow map —
    // there's no instanced shadow shader yet.
    let batch_keys: Vec<(engine_utils::AssetId, engine_utils::AssetId)> = visible
        .iter()
        .map(|mesh| (mesh.mesh_handle.id(), mesh.material_handle.id()))
        .collect();
    let plan = plan_auto_batches(&batch_keys);

    // Counted before batching appends to `pending_instanced`, so it stays
    // "explicit `InstancedMeshRenderer` instances only".
    let instances_drawn: u32 = pending_instanced
        .iter()
        .map(|pending| pending.raws.len() as u32)
        .sum();

    let mut drawables: Vec<Drawable<'_>> = Vec::with_capacity(plan.individuals.len());
    for &index in &plan.individuals {
        let mesh = &visible[index];
        drawables.push(Drawable {
            model: mesh.model,
            material: mesh.material,
            mesh: mesh.mesh,
        });
    }

    let auto_batches = plan.batches.len() as u32;
    let mut auto_batched_drawn = 0u32;
    for group in &plan.batches {
        let first = &visible[group[0]];
        let raws: Vec<InstanceRaw> = group
            .iter()
            .map(|&index| InstanceRaw {
                model: visible[index].world.to_cols_array_2d(),
            })
            .collect();
        auto_batched_drawn += raws.len() as u32;
        pending_instanced.push(PendingInstanced {
            material: first.material,
            mesh: first.mesh,
            raws,
        });
    }

    // Buffers first (must outlive the render pass that borrows them), then
    // the drawables that point at them. `pending_instanced` now holds the
    // explicit `InstancedMeshRenderer` draws followed by the batched ones.
    let instance_buffers: Vec<InstanceBuffer> = pending_instanced
        .iter()
        .map(|pending| gpu.create_instance_buffer("instanced mesh instances", &pending.raws))
        .collect();
    let instanced_drawables: Vec<InstancedDrawable<'_>> = pending_instanced
        .iter()
        .zip(&instance_buffers)
        .map(|(pending, buffer)| InstancedDrawable {
            material: pending.material,
            mesh: pending.mesh,
            instance_buffer: buffer,
            instance_count: pending.raws.len() as u32,
        })
        .collect();

    // Vegetation: the same per-instance cull + instance-buffer path as
    // `InstancedMeshRenderer`, but drawn through the wind pipeline. Skipped
    // entirely (and never queried) when the caller passes no vegetation
    // pipeline/wind binding.
    let mut vegetation_total = 0u32;
    let mut pending_vegetation: Vec<PendingInstanced<'_>> = Vec::new();
    if vegetation.is_some() {
        for plants in vegetation_query.iter(&*world) {
            let Some(mesh) = assets.meshes.get(plants.mesh) else {
                tracing::warn!(
                    mesh = ?plants.mesh,
                    "skipping VegetationRenderer: mesh handle not registered in RenderAssets"
                );
                continue;
            };
            let Some(material) = assets.materials.get(plants.material) else {
                tracing::warn!(
                    material = ?plants.material,
                    "skipping VegetationRenderer: material handle not registered in RenderAssets"
                );
                continue;
            };
            vegetation_total += plants.instances.len() as u32;
            let raws = visible_instances(frustum, &mesh.local_bounds, &plants.instances);
            if !raws.is_empty() {
                pending_vegetation.push(PendingInstanced {
                    material,
                    mesh,
                    raws,
                });
            }
        }
    }
    let vegetation_drawn: u32 = pending_vegetation
        .iter()
        .map(|pending| pending.raws.len() as u32)
        .sum();
    let vegetation_instance_buffers: Vec<InstanceBuffer> = pending_vegetation
        .iter()
        .map(|pending| gpu.create_instance_buffer("vegetation instances", &pending.raws))
        .collect();
    let vegetation_drawables: Vec<InstancedDrawable<'_>> = pending_vegetation
        .iter()
        .zip(&vegetation_instance_buffers)
        .map(|(pending, buffer)| InstancedDrawable {
            material: pending.material,
            mesh: pending.mesh,
            instance_buffer: buffer,
            instance_count: pending.raws.len() as u32,
        })
        .collect();

    // Borrowed after the batch itself, so the frame's instance slice
    // outlives the render call that reads it.
    let sprite_frame = match (sprites, &sprite_batch) {
        (Some(target), Some(batch)) if !batch.is_empty() => Some(SpriteFrame {
            pipeline: target.pipeline,
            quad: target.quad,
            atlas: target.atlas,
            instances: batch.instances(),
        }),
        _ => None,
    };

    let stats = RenderStats {
        meshes_total: shadow_casters.len() as u32,
        meshes_drawn: visible.len() as u32,
        skinned_drawn: skinned_drawables.len() as u32,
        instances_total,
        instances_drawn,
        auto_batches,
        auto_batched_drawn,
        vegetation_total,
        vegetation_drawn,
        sprites_drawn: sprite_frame.map_or(0, |frame| frame.instances.len() as u32),
    };

    let vegetation_frame =
        vegetation.map(|(pipeline, wind)| (pipeline, wind, vegetation_drawables.as_slice()));

    match target {
        Some(target) => gpu.render_scene_to(
            target,
            pipeline,
            shadow_pipeline,
            shadow_map,
            skybox_pipeline,
            skybox,
            hdr_target,
            post,
            camera,
            lights,
            &drawables,
            &shadow_casters,
            &transparent,
            skinned_pipeline,
            &skinned_drawables,
            instanced_pipeline,
            &instanced_drawables,
            debug_lines,
            sprite_frame,
            particles,
            vegetation_frame,
            ui,
        )?,
        None => gpu.render_scene(
            pipeline,
            shadow_pipeline,
            shadow_map,
            skybox_pipeline,
            skybox,
            hdr_target,
            post,
            camera,
            lights,
            &drawables,
            &shadow_casters,
            &transparent,
            skinned_pipeline,
            &skinned_drawables,
            instanced_pipeline,
            &instanced_drawables,
            debug_lines,
            sprite_frame,
            particles,
            vegetation_frame,
            ui,
        )?,
    }

    Ok(stats)
}

/// Collects every `(GlobalTransform, Sprite)` entity into one
/// [`SpriteBatch`], in draw order.
///
/// Transforms must already be propagated — [`extract_and_render_to`]
/// does that before calling this, and a standalone caller should call
/// [`propagate_transforms`] first.
///
/// # Draw order
///
/// Depth is tested against the scene but not written (a sprite is
/// alpha-blended), so sprite-versus-sprite order is decided here, not by
/// the GPU. Three keys, in order:
///
/// 1. [`Sprite::z_order`] ascending — higher layers draw last, on top.
/// 2. View depth descending — farther sprites first, so nearer ones
///    blend over them. This is what a 3D scene's billboards want, and it
///    is the only key that moves when every `z_order` is the default
///    `0.0`.
/// 3. Entity id ascending — never a visual choice, only a tie-break so
///    the order does not change between frames (ECS iteration order is
///    not a promise) or between runs.
///
/// A sprite entity's world-space rotation comes from
/// [`crate::components::Transform`]'s quaternion via
/// [`glam::Quat::to_scaled_axis`]'s `z` component — exact for a pure
/// rotation about `Z` (the only kind a 2D game produces; anything tilting
/// out of the XY plane isn't representable by a flat sprite's single
/// rotation angle anyway).
pub fn extract_sprites(world: &mut World, frustum: &Frustum) -> SpriteBatch {
    // `(z_order, view depth, entity id, instance)` — the sort keys
    // carried alongside so the comparison touches no component data.
    let mut sorted: Vec<(f32, f32, u64, SpriteInstance)> = Vec::new();
    let mut query = world.query::<(Entity, &GlobalTransform, &Sprite)>();
    for (entity, transform, sprite) in query.iter(&*world) {
        let rotation = transform.0.rotation.to_scaled_axis().z;
        sorted.push((
            sprite.z_order,
            view_depth(frustum, transform.0.translation),
            entity.to_bits(),
            SpriteInstance {
                position: transform.0.translation.to_array(),
                size: sprite.size.to_array(),
                rotation,
                uv_min: sprite.uv.min,
                uv_max: sprite.uv.max,
                color: sprite.color,
            },
        ));
    }

    // `total_cmp`, not `partial_cmp`: a NaN `z_order` from game code must
    // not panic the frame, and a total order keeps the sort well-defined.
    sorted.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| b.1.total_cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut batch = SpriteBatch::new();
    for (_, _, _, instance) in sorted {
        batch.push(instance);
    }
    batch
}

/// Despawns `entity`, first releasing its [`MeshRenderer`]'s `mesh`/
/// `material` handles from `assets` (if it has one) so a mesh or material
/// still shared with other entities survives, and one that isn't gets
/// freed.
///
/// Use this instead of a bare `World::despawn` for any entity that might
/// carry a `MeshRenderer` — mirrors [`crate::components::RigidBody`]/
/// [`crate::components::Collider`]'s existing shape, where releasing a
/// handle-backed component's underlying resource is the caller's explicit
/// job, not something `bevy_ecs` does automatically on despawn.
///
/// Returns whatever `World::despawn` returns: `true` if `entity` existed
/// (and was despawned), `false` if it didn't.
pub fn despawn_mesh_renderer(world: &mut World, entity: Entity, assets: &mut RenderAssets) -> bool {
    if let Some(mesh_renderer) = world.get::<MeshRenderer>(entity) {
        assets.meshes.release(mesh_renderer.mesh);
        assets.materials.release(mesh_renderer.material);
    }
    world.despawn(entity)
}

/// The [`SkinnedMeshRenderer`] counterpart to [`despawn_mesh_renderer`]:
/// despawns `entity`, first releasing its skinned `mesh` and `material`
/// handles from `assets` (if it has the component).
///
/// Returns whatever `World::despawn` returns.
pub fn despawn_skinned_mesh_renderer(
    world: &mut World,
    entity: Entity,
    assets: &mut RenderAssets,
) -> bool {
    if let Some(skinned) = world.get::<SkinnedMeshRenderer>(entity) {
        assets.skinned_meshes.release(skinned.mesh);
        assets.materials.release(skinned.material);
    }
    world.despawn(entity)
}

/// The [`InstancedMeshRenderer`] counterpart: despawns `entity`, first
/// releasing its shared `mesh` and `material` handles once each (there are
/// no per-instance GPU handles).
///
/// Returns whatever `World::despawn` returns.
pub fn despawn_instanced_mesh_renderer(
    world: &mut World,
    entity: Entity,
    assets: &mut RenderAssets,
) -> bool {
    if let Some(instanced) = world.get::<InstancedMeshRenderer>(entity) {
        assets.meshes.release(instanced.mesh);
        assets.materials.release(instanced.material);
    }
    world.despawn(entity)
}

/// The [`VegetationRenderer`] counterpart: despawns `entity`, first
/// releasing its shared `mesh` and `material` handles once each.
///
/// Returns whatever `World::despawn` returns.
pub fn despawn_vegetation_renderer(
    world: &mut World,
    entity: Entity,
    assets: &mut RenderAssets,
) -> bool {
    if let Some(plants) = world.get::<VegetationRenderer>(entity) {
        assets.meshes.release(plants.mesh);
        assets.materials.release(plants.material);
    }
    world.despawn(entity)
}

#[cfg(test)]
mod tests {
    use super::view_depth;

    #[test]
    fn view_depth_grows_with_distance_from_the_camera() {
        // Camera at the origin looking down -Z.
        let camera =
            engine_renderer::Camera::new(glam::Vec3::ZERO, glam::Vec3::new(0.0, 0.0, -1.0), 1.0);
        let frustum = camera.frustum();

        let near_point = glam::Vec3::new(0.0, 0.0, -5.0);
        let far_point = glam::Vec3::new(0.0, 0.0, -50.0);

        assert!(
            view_depth(&frustum, far_point) > view_depth(&frustum, near_point),
            "a farther point must sort as deeper, or back-to-front ordering inverts",
        );
    }

    #[test]
    fn transparent_sort_is_back_to_front() {
        let camera =
            engine_renderer::Camera::new(glam::Vec3::ZERO, glam::Vec3::new(0.0, 0.0, -1.0), 1.0);
        let frustum = camera.frustum();

        let mut depths: Vec<f32> = [-5.0, -50.0, -20.0]
            .into_iter()
            .map(|z| view_depth(&frustum, glam::Vec3::new(0.0, 0.0, z)))
            .collect();
        // The exact comparator the render path uses.
        depths.sort_by(|a, b| b.total_cmp(a));

        assert!(
            depths.windows(2).all(|w| w[0] >= w[1]),
            "sorted order must run farthest to nearest",
        );
    }

    use super::*;
    use engine_renderer::Camera;
    use glam::Vec3;

    fn view_frustum() -> Frustum {
        Camera::new(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, 1.0).frustum()
    }

    fn unit_bounds() -> Aabb {
        Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        }
    }

    /// A world holding one sprite per `(z_order, z position)` pair, with
    /// its `GlobalTransform` already set — `extract_sprites` reads that,
    /// not `Transform`, and does not propagate.
    fn world_with_sprites(sprites: &[(f32, f32)]) -> World {
        let uv = engine_renderer::UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        let mut world = World::new();
        for &(z_order, z) in sprites {
            let mut sprite = Sprite::new(glam::Vec2::ONE, uv);
            sprite.z_order = z_order;
            world.spawn((
                GlobalTransform(engine_utils::Transform::from_translation(Vec3::new(
                    0.0, 0.0, z,
                ))),
                sprite,
            ));
        }
        world
    }

    #[test]
    fn sprite_batch_sorts_back_to_front() {
        // Camera at z = 5 looking at the origin, so a smaller z is
        // farther away and must be drawn first.
        let mut world = world_with_sprites(&[(0.0, 2.0), (0.0, -4.0), (0.0, 0.0)]);
        let batch = extract_sprites(&mut world, &view_frustum());

        let order: Vec<f32> = batch
            .instances()
            .iter()
            .map(|instance| instance.position[2])
            .collect();
        assert_eq!(
            order,
            vec![-4.0, 0.0, 2.0],
            "sprites on one layer must draw farthest-first so nearer ones blend over them",
        );
    }

    #[test]
    fn z_order_outranks_view_depth() {
        // The nearer sprite (z = 4) is on the lower layer, so it draws
        // first even though depth alone would put it last.
        let mut world = world_with_sprites(&[(1.0, -4.0), (0.0, 4.0)]);
        let batch = extract_sprites(&mut world, &view_frustum());

        let order: Vec<f32> = batch
            .instances()
            .iter()
            .map(|instance| instance.position[2])
            .collect();
        assert_eq!(
            order,
            vec![4.0, -4.0],
            "z_order is the primary key: a low layer draws under a high one at any depth",
        );
    }

    #[test]
    fn z_order_breaks_ties_deterministically() {
        // Same layer, same position: nothing separates these but the
        // entity id, and the order must not wander between calls.
        let mut world = world_with_sprites(&[(0.0, 0.0), (0.0, 0.0), (0.0, 0.0)]);
        let frustum = view_frustum();

        let first = extract_sprites(&mut world, &frustum);
        let second = extract_sprites(&mut world, &frustum);
        assert_eq!(
            first.instances(),
            second.instances(),
            "two extracts of an unchanged world must produce the same draw order",
        );
        assert_eq!(first.len(), 3);
    }

    #[test]
    fn empty_sprite_set_skips_the_pass() {
        // No sprite entities at all: the batch is empty, which is what
        // `extract_and_render_to` tests before building a `SpriteFrame` —
        // no instance buffer, no pipeline switch, no draw.
        let mut world = World::new();
        let batch = extract_sprites(&mut world, &view_frustum());
        assert!(batch.is_empty());
        assert_eq!(batch.len(), 0);
    }

    #[test]
    fn a_nan_z_order_does_not_panic_the_sort() {
        let mut world = world_with_sprites(&[(f32::NAN, 0.0), (0.0, 1.0), (1.0, -1.0)]);
        let batch = extract_sprites(&mut world, &view_frustum());
        assert_eq!(batch.len(), 3, "every sprite must survive a NaN layer");
    }

    #[test]
    fn is_visible_true_for_a_mesh_at_the_look_at_point() {
        assert!(is_visible(&view_frustum(), &unit_bounds(), &Mat4::IDENTITY));
    }

    #[test]
    fn is_visible_false_for_a_mesh_translated_out_of_view() {
        let far_to_the_side = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
        assert!(!is_visible(
            &view_frustum(),
            &unit_bounds(),
            &far_to_the_side
        ));
    }

    #[test]
    fn is_visible_true_when_scale_stretches_a_mesh_back_into_view() {
        // Centered at x = 100 (out of view), but scaled 400x on X so the
        // box spans x in [-100, 300] and crosses the frustum.
        let stretched = Mat4::from_scale_rotation_translation(
            Vec3::new(400.0, 1.0, 1.0),
            glam::Quat::IDENTITY,
            Vec3::new(100.0, 0.0, 0.0),
        );
        assert!(is_visible(&view_frustum(), &unit_bounds(), &stretched));
    }

    #[test]
    fn despawn_mesh_renderer_without_the_component_just_despawns() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut assets = RenderAssets::new();

        assert!(despawn_mesh_renderer(&mut world, entity, &mut assets));
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn despawn_mesh_renderer_on_a_missing_entity_returns_false() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        world.despawn(entity);
        let mut assets = RenderAssets::new();

        assert!(!despawn_mesh_renderer(&mut world, entity, &mut assets));
    }

    #[test]
    fn despawn_skinned_mesh_renderer_without_the_component_just_despawns() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut assets = RenderAssets::new();

        assert!(despawn_skinned_mesh_renderer(
            &mut world,
            entity,
            &mut assets
        ));
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn despawn_skinned_mesh_renderer_on_a_missing_entity_returns_false() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        world.despawn(entity);
        let mut assets = RenderAssets::new();

        assert!(!despawn_skinned_mesh_renderer(
            &mut world,
            entity,
            &mut assets
        ));
    }

    #[test]
    fn visible_instances_keeps_all_when_all_in_view() {
        let instances = [
            engine_utils::Transform::from_translation(Vec3::ZERO),
            engine_utils::Transform::from_translation(Vec3::new(0.5, 0.0, 0.0)),
            engine_utils::Transform::from_translation(Vec3::new(-0.5, 0.0, 0.5)),
        ];
        let raws = visible_instances(&view_frustum(), &unit_bounds(), &instances);
        assert_eq!(raws.len(), 3);
    }

    #[test]
    fn visible_instances_drops_the_ones_outside_the_frustum() {
        let instances = [
            engine_utils::Transform::from_translation(Vec3::ZERO),
            engine_utils::Transform::from_translation(Vec3::new(100.0, 0.0, 0.0)),
            engine_utils::Transform::from_translation(Vec3::new(0.0, 0.0, 50.0)),
        ];
        let raws = visible_instances(&view_frustum(), &unit_bounds(), &instances);
        assert_eq!(raws.len(), 1);
        assert_eq!(
            raws[0].model,
            engine_utils::Transform::from_translation(Vec3::ZERO)
                .to_matrix()
                .to_cols_array_2d()
        );
    }

    #[test]
    fn visible_instances_of_an_empty_set_is_empty() {
        assert!(visible_instances(&view_frustum(), &unit_bounds(), &[]).is_empty());
    }

    fn fresh_ids<const N: usize>() -> [engine_utils::AssetId; N] {
        std::array::from_fn(|_| engine_utils::AssetId::new())
    }

    #[test]
    fn plan_auto_batches_of_nothing_is_an_empty_plan() {
        assert_eq!(plan_auto_batches(&[]), BatchPlan::default());
    }

    #[test]
    fn plan_auto_batches_all_distinct_keys_are_all_individual() {
        let [a, b, c] = fresh_ids();
        let plan = plan_auto_batches(&[(a, a), (b, b), (c, c)]);
        assert_eq!(plan.individuals, vec![0, 1, 2]);
        assert!(plan.batches.is_empty());
    }

    #[test]
    fn plan_auto_batches_one_shared_key_becomes_a_single_batch() {
        let [mesh, material] = fresh_ids();
        let key = (mesh, material);
        let plan = plan_auto_batches(&[key, key, key, key]);
        assert!(plan.individuals.is_empty());
        assert_eq!(plan.batches, vec![vec![0, 1, 2, 3]]);
    }

    #[test]
    fn plan_auto_batches_threshold_is_exactly_two() {
        assert_eq!(MIN_AUTO_BATCH_SIZE, 2);
        let [solo_mesh, pair_mesh] = fresh_ids();
        let solo = (solo_mesh, solo_mesh);
        let pair = (pair_mesh, pair_mesh);
        let plan = plan_auto_batches(&[solo, pair, pair]);
        assert_eq!(plan.individuals, vec![0]);
        assert_eq!(plan.batches, vec![vec![1, 2]]);
    }

    #[test]
    fn plan_auto_batches_needs_both_mesh_and_material_to_match() {
        let [mesh_a, mesh_b, mat_a, mat_b] = fresh_ids();
        // same mesh, different material -> not grouped.
        let same_mesh = plan_auto_batches(&[(mesh_a, mat_a), (mesh_a, mat_b)]);
        assert_eq!(same_mesh.individuals, vec![0, 1]);
        assert!(same_mesh.batches.is_empty());
        // different mesh, same material -> not grouped.
        let same_material = plan_auto_batches(&[(mesh_a, mat_a), (mesh_b, mat_a)]);
        assert_eq!(same_material.individuals, vec![0, 1]);
        assert!(same_material.batches.is_empty());
    }

    #[test]
    fn plan_auto_batches_keeps_first_seen_order_for_groups_and_members() {
        let [a, b, c, d] = fresh_ids();
        let crate_key = (a, b);
        let barrel_key = (c, d);
        let lone_1 = (a, a);
        let lone_2 = (b, b);
        // encounter: crate, barrel, lone1, crate, barrel, lone2, crate
        let plan = plan_auto_batches(&[
            crate_key, barrel_key, lone_1, crate_key, barrel_key, lone_2, crate_key,
        ]);
        // batches in first-seen order (crate before barrel), members in
        // encounter order.
        assert_eq!(plan.batches, vec![vec![0, 3, 6], vec![1, 4]]);
        assert_eq!(plan.individuals, vec![2, 5]);
    }

    #[test]
    fn despawn_instanced_mesh_renderer_without_the_component_just_despawns() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut assets = RenderAssets::new();

        assert!(despawn_instanced_mesh_renderer(
            &mut world,
            entity,
            &mut assets
        ));
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn despawn_instanced_mesh_renderer_on_a_missing_entity_returns_false() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        world.despawn(entity);
        let mut assets = RenderAssets::new();

        assert!(!despawn_instanced_mesh_renderer(
            &mut world,
            entity,
            &mut assets
        ));
    }

    #[test]
    fn despawn_vegetation_renderer_without_the_component_just_despawns() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        let mut assets = RenderAssets::new();

        assert!(despawn_vegetation_renderer(&mut world, entity, &mut assets));
        assert!(world.get_entity(entity).is_err());
    }

    #[test]
    fn despawn_vegetation_renderer_on_a_missing_entity_returns_false() {
        let mut world = World::new();
        let entity = world.spawn(()).id();
        world.despawn(entity);
        let mut assets = RenderAssets::new();

        assert!(!despawn_vegetation_renderer(
            &mut world,
            entity,
            &mut assets
        ));
    }

    #[test]
    fn render_stats_default_has_zero_vegetation_counts() {
        let stats = RenderStats::default();
        assert_eq!(stats.vegetation_total, 0);
        assert_eq!(stats.vegetation_drawn, 0);
        // Copy + Eq still hold with the new fields.
        let copy = stats;
        assert_eq!(stats, copy);
    }
}
