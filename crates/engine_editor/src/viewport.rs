//! The Scene view: the editor's world rendered off-screen into a texture
//! that an egui panel displays.
//!
//! ## The same renderer a shipped game uses
//!
//! This draws through [`engine_ecs::extract_and_render_to`] — the exact
//! call [`engine::app`]'s frame loop makes, with the exact PBR / shadow /
//! skybox / HDR / post-processing stack. What the editor shows is what
//! the game will draw.
//!
//! It did not always work that way: until task T-03 this module owned a
//! toy pipeline that drew *every* entity as the same untextured grey
//! cube, capped at 256 of them, with no depth buffer. Geometry, material,
//! lighting and draw order were all fiction. Everything below exists to
//! not be that.
//!
//! ## Off-screen, not the swapchain
//!
//! `egui_wgpu::Renderer::register_native_texture` accepts exactly one
//! format ([`VIEWPORT_TEXTURE_FORMAT`]), and the window's swapchain
//! belongs to egui, not to us. So the viewport owns its own
//! [`HdrTarget`] and a [`PostProcessStack`] built for that format
//! (`create_post_process_stack_for_format`), and composites into its own
//! texture via [`engine_renderer::RenderTarget`].
//!
//! ## Overlays
//!
//! The translate/rotate/scale gizmo, and a wireframe box for any entity
//! with no resolvable mesh, both ride the renderer's existing
//! `debug_lines` channel rather than a private pipeline — one line list,
//! drawn in the scene pass, depth-configured to stay visible through
//! geometry so a gizmo behind a wall is still grabbable.

use engine_ecs::components::{
    Disabled, GlobalTransform, InstancedMeshRenderer, MeshRenderer, Sprite, SpriteSource,
};
use engine_ecs::prelude::{Entity, World};
use engine_ecs::render::SpriteTarget;
use engine_renderer::{
    Camera, CameraBinding, DebugLinePipeline, DebugLineVertex, DirectionalLight, GpuContext,
    HdrTarget, InstancedPipeline, LightSet, LightsBinding, Pipeline, PostProcessStack,
    PostSettings, RenderAssets, RenderTarget, ShadowMap, ShadowPipeline, SkinnedPipeline,
    SkyboxBinding, SkyboxPipeline, SpriteAtlasBinding, SpritePipeline, TextureAtlas, WaterBinding,
    WaterDraw, WaterFrame, WaterGpuUniform, WaterPipeline, directional_light_view_projection,
    skybox_uniform,
};
use engine_utils::AssetHandle;
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;

use crate::error::EditorError;
use crate::shell::EditorShell;

/// `egui_wgpu::Renderer::register_native_texture` requires exactly this
/// format for a texture it's asked to display.
pub const VIEWPORT_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Half-extent of the wireframe box drawn for an entity with no
/// resolvable mesh, in world units. Big enough to click, small enough not
/// to swamp real geometry.
const PLACEHOLDER_HALF_EXTENT: f32 = 0.5;

type InstancedHandles = (
    AssetHandle<engine_renderer::Mesh>,
    AssetHandle<engine_renderer::MaterialBinding>,
);

fn instanced_handles(world: &mut World) -> HashMap<Entity, InstancedHandles> {
    world
        .query::<(Entity, &InstancedMeshRenderer)>()
        .iter(world)
        .map(|(entity, renderer)| (entity, (renderer.mesh, renderer.material)))
        .collect()
}

fn removed_instanced_handles(
    before: &HashMap<Entity, InstancedHandles>,
    after: &HashMap<Entity, InstancedHandles>,
) -> Vec<InstancedHandles> {
    before
        .iter()
        .filter_map(|(entity, handles)| (after.get(entity) != Some(handles)).then_some(*handles))
        .collect()
}

/// Colour of that wireframe box: warning red, because a model that should
/// be here did not load.
const PLACEHOLDER_COLOR: [f32; 4] = [0.95, 0.3, 0.28, 1.0];

/// The 12 edges of a unit cube, as index pairs into
/// [`box_corners`]' output.
const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0),
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

/// The eight corners of an axis-aligned box of `half_extent`, centred on
/// `centre`, in the order [`BOX_EDGES`] indexes.
fn box_corners(centre: Vec3, half_extent: f32) -> [Vec3; 8] {
    let mut corners = [Vec3::ZERO; 8];
    for (i, corner) in corners.iter_mut().enumerate() {
        let sign = |bit: usize| if i & (1 << bit) == 0 { -1.0 } else { 1.0 };
        *corner = centre + Vec3::new(sign(0), sign(1), sign(2)) * half_extent;
    }
    corners
}

/// Appends a wireframe box to `out` as line-list vertices.
fn push_wire_box(out: &mut Vec<DebugLineVertex>, centre: Vec3, half_extent: f32, color: [f32; 4]) {
    let corners = box_corners(centre, half_extent);
    for &(a, b) in &BOX_EDGES {
        out.push(DebugLineVertex {
            position: corners[a].into(),
            color,
        });
        out.push(DebugLineVertex {
            position: corners[b].into(),
            color,
        });
    }
}

/// Builds the frame's placeholder line list: a wireframe box per entity
/// with no resolvable mesh. (The transform gizmo is painted by the shell
/// over the viewport image, so it is always on top.)
///
/// Pure over its inputs so it can be unit-tested without a GPU.
fn overlay_lines(placeholders: &[Vec3]) -> Vec<DebugLineVertex> {
    let mut lines = Vec::new();
    for &centre in placeholders {
        push_wire_box(
            &mut lines,
            centre,
            PLACEHOLDER_HALF_EXTENT,
            PLACEHOLDER_COLOR,
        );
    }
    lines
}

/// The editor's off-screen scene render.
///
/// The render target follows the visible panel size so the scene fills
/// the Scene and Code viewports without stretching its camera aspect.
pub struct Viewport {
    width: u32,
    height: u32,
    view: wgpu::TextureView,
    texture_id: egui::TextureId,
    camera: Camera,
    /// `true` while the 2D authoring camera / gizmo conventions are
    /// active — see [`Viewport::set_dimension`].
    two_d: bool,

    // The real renderer, owned per-viewport because it targets our
    // off-screen texture rather than the window.
    pipeline: Pipeline,
    shadow_pipeline: ShadowPipeline,
    shadow_map: ShadowMap,
    skinned_pipeline: SkinnedPipeline,
    instanced_pipeline: InstancedPipeline,
    sprite_pipeline: SpritePipeline,
    sprite_quad: engine_renderer::Mesh,
    sprite_atlas: Option<SpriteAtlasBinding>,
    sprite_layout: Option<engine_renderer::AtlasLayout>,
    sprite_ids: Vec<String>,
    sprite_import_pass: u64,
    water_pipeline: WaterPipeline,
    water_bindings: HashMap<Entity, WaterBinding>,
    skybox_pipeline: SkyboxPipeline,
    skybox: SkyboxBinding,
    debug_line_pipeline: DebugLinePipeline,
    /// The frame's debug geometry, rebuilt every frame and kept between
    /// them for its allocation.
    debug_draw: engine_renderer::DebugDraw,
    hdr_target: HdrTarget,
    post: PostProcessStack,
    camera_binding: CameraBinding,
    lights_binding: LightsBinding,
    lights: LightSet,
    /// GPU meshes/materials the scene's entities resolved to. Owned here
    /// because the entities' `MeshRenderer` handles index into it.
    assets: RenderAssets,
    /// Handles owned by live instanced groups at the last editor boundary.
    tracked_instanced: HashMap<Entity, InstancedHandles>,
}

fn default_editor_lights() -> LightSet {
    LightSet {
        directional: vec![DirectionalLight {
            direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
            color: Vec3::ONE,
            intensity: 3.0,
        }],
        point: Vec::new(),
        ambient: engine_renderer::AmbientLight::DAYLIGHT,
    }
}

fn editor_lights(world: &mut World) -> LightSet {
    let mut lights = world
        .get_resource::<engine_scene::SceneLighting>()
        .map(|lighting| lighting.0.to_light_set())
        .unwrap_or_else(default_editor_lights);
    lights
        .point
        .extend(engine_scene::point_lights_from_world(world));
    lights
}

impl Viewport {
    /// Creates a `width` by `height` Scene view and registers its texture
    /// with `shell`'s egui renderer.
    ///
    /// # Errors
    ///
    /// [`EditorError::Renderer`] if any GPU resource fails to build.
    pub fn new(
        gpu: &GpuContext,
        shell: &mut EditorShell,
        width: u32,
        height: u32,
    ) -> Result<Self, EditorError> {
        let device = gpu.device();

        let view = Self::texture_view(device, width, height);
        let texture_id = shell.register_texture(device, &view);

        let pipeline = gpu.create_pbr_pipeline("editor pbr");
        let shadow_pipeline = gpu.create_shadow_pipeline(&pipeline, "editor shadow");
        let skinned_pipeline = gpu.create_skinned_pbr_pipeline(&pipeline, "editor skinned pbr");
        let instanced_pipeline =
            gpu.create_instanced_pbr_pipeline(&pipeline, "editor instanced pbr");
        let sprite_pipeline = gpu.create_sprite_pipeline("editor sprites", &pipeline);
        let (sprite_vertices, sprite_indices) = engine_renderer::quad();
        let sprite_quad =
            gpu.create_mesh("editor sprite quad", &sprite_vertices, &sprite_indices)?;
        let water_pipeline = gpu.create_water_pipeline(&pipeline, "editor water")?;
        let skybox_pipeline = gpu.create_skybox_pipeline("editor skybox");
        let debug_line_pipeline = gpu.create_debug_line_pipeline(&pipeline, "editor overlay lines");

        let camera =
            crate::SceneCamera::default().to_camera(false, width as f32 / height.max(1) as f32);
        // One sun, angled down and slightly forward so authored geometry
        // reads as solid rather than flat-lit.
        let lights = default_editor_lights();
        let (sun_direction, sun_color) = lights
            .directional
            .first()
            .map(|light| (light.direction, light.color))
            .unwrap_or((Vec3::NEG_Y, Vec3::ONE));

        let light_space = directional_light_view_projection(sun_direction, Vec3::ZERO, 16.0);
        let shadow_map = gpu.create_shadow_map(&shadow_pipeline, light_space);
        let camera_binding = gpu.create_camera_binding(&pipeline, &camera.to_uniform());
        let lights_binding =
            gpu.create_lights_binding(&pipeline, &lights.to_uniform(), &shadow_map);
        let skybox = gpu.create_skybox_binding(
            &skybox_pipeline,
            &skybox_uniform(&camera, sun_direction, sun_color),
        );

        let hdr_target = gpu.create_hdr_target(width, height);
        // Composites into our own texture's format, not the window's.
        let post = gpu.create_post_process_stack_for_format(
            &hdr_target,
            width,
            height,
            PostSettings::default(),
            VIEWPORT_TEXTURE_FORMAT,
        );

        Ok(Self {
            width,
            height,
            view,
            texture_id,
            camera,
            two_d: false,
            pipeline,
            shadow_pipeline,
            shadow_map,
            skinned_pipeline,
            instanced_pipeline,
            sprite_pipeline,
            sprite_quad,
            sprite_atlas: None,
            sprite_layout: None,
            sprite_ids: Vec::new(),
            sprite_import_pass: 0,
            water_pipeline,
            water_bindings: HashMap::new(),
            skybox_pipeline,
            skybox,
            debug_line_pipeline,
            debug_draw: engine_renderer::DebugDraw::new(),
            hdr_target,
            post,
            camera_binding,
            lights_binding,
            lights,
            assets: RenderAssets::new(),
            tracked_instanced: HashMap::new(),
        })
    }

    /// Rebuilds the GPU camera from the editor's navigation state for the
    /// current 2D/3D mode, uploading only when the view actually changed.
    pub fn sync_camera(&mut self, gpu: &GpuContext, view: &crate::SceneCamera, two_d: bool) {
        self.two_d = two_d;
        let camera = view.to_camera(two_d, self.width as f32 / self.height.max(1) as f32);
        if camera.is_valid() && camera != self.camera {
            self.camera = camera;
            gpu.write_uniform_buffer(&self.camera_binding.buffer, &self.camera.to_uniform());
        }
    }

    /// This viewport's pixel dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Resizes the scene target while retaining its texture id and GPU
    /// asset cache. The host calls this after presenting the old frame.
    pub fn resize(&mut self, gpu: &GpuContext, shell: &mut EditorShell, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return;
        }
        let device = gpu.device();
        let view = Self::texture_view(device, width, height);
        let hdr_target = gpu.create_hdr_target(width, height);
        let post = gpu.create_post_process_stack_for_format(
            &hdr_target,
            width,
            height,
            PostSettings::default(),
            VIEWPORT_TEXTURE_FORMAT,
        );
        shell.update_texture(device, self.texture_id, &view);
        self.view = view;
        self.hdr_target = hdr_target;
        self.post = post;
        self.width = width;
        self.height = height;
        self.camera.aspect_ratio = width as f32 / height as f32;
        gpu.write_uniform_buffer(&self.camera_binding.buffer, &self.camera.to_uniform());
    }

    fn texture_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("editor viewport target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: VIEWPORT_TEXTURE_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// The egui texture handle this viewport's panel displays.
    pub fn texture_id(&self) -> egui::TextureId {
        self.texture_id
    }

    /// The camera's combined view-projection, for screen-space gizmo
    /// picking.
    pub fn view_projection_matrix(&self) -> Mat4 {
        self.camera.view_projection_matrix()
    }

    /// Drops every mesh, material and texture this viewport has
    /// uploaded.
    ///
    /// Called when the studio switches projects: the handles belong to
    /// files that are no longer open, and keeping them would both waste
    /// GPU memory and let the new project draw the old one's geometry
    /// wherever an id happened to collide.
    pub fn release_assets(&mut self) {
        self.assets = RenderAssets::new();
        self.tracked_instanced.clear();
        self.sprite_atlas = None;
        self.sprite_layout = None;
        self.sprite_ids.clear();
        self.sprite_import_pass = 0;
    }

    /// How many meshes this viewport currently holds — the hook a test
    /// uses to prove a project swap actually released them.
    pub fn uploaded_mesh_count(&self) -> usize {
        self.assets.meshes.len()
    }

    /// Reconciles instanced GPU ownership after a hierarchy or scene edit.
    /// Deleting a group removes its ECS component but cannot release the
    /// viewport-owned GPU handles at the deletion call site.
    pub fn sync_instanced_handles(&mut self, world: &mut World) {
        let current = instanced_handles(world);
        for (mesh, material) in removed_instanced_handles(&self.tracked_instanced, &current) {
            self.assets.meshes.release(mesh);
            self.assets.materials.release(material);
        }
        self.tracked_instanced = current;
    }

    /// Resolves any of `world`'s renderable references that are now
    /// importable into live components, uploading into this viewport's
    /// own asset store.
    ///
    /// A method rather than a free function because the pipeline the
    /// uploads are built against and the store they land in both belong
    /// to this viewport — passing them out separately would borrow it
    /// twice. See [`crate::resolve_pending`] for what a pass does.
    pub fn resolve_pending(
        &mut self,
        world: &mut World,
        importer: &crate::AssetImporter,
        gpu: &GpuContext,
    ) -> crate::ResolveReport {
        let report = crate::resolve_pending(world, importer, gpu, &self.pipeline, &mut self.assets);
        self.refresh_sprite_atlas(world, importer, gpu);
        report
    }

    fn refresh_sprite_atlas(
        &mut self,
        world: &mut World,
        importer: &crate::AssetImporter,
        gpu: &GpuContext,
    ) {
        let mut ids: Vec<String> = world
            .query::<(&SpriteSource, &Sprite)>()
            .iter(world)
            .map(|(source, _)| source.atlas.clone())
            .collect();
        ids.sort();
        ids.dedup();
        if self.sprite_ids == ids && self.sprite_import_pass == importer.pass_id() {
            engine_scene::apply_packed_sprite_uvs(world, self.sprite_layout.as_ref());
            return;
        }
        self.sprite_ids = ids;
        self.sprite_import_pass = importer.pass_id();
        let images: Vec<_> = self
            .sprite_ids
            .iter()
            .filter_map(|id| {
                let asset_id = crate::assets::parse_asset_id(id)?;
                match importer.get(asset_id)? {
                    crate::ImportedAsset::Texture(image) => Some((id.as_str(), image.as_ref())),
                    _ => None,
                }
            })
            .collect();
        match engine_scene::pack_sprite_textures(
            &images,
            gpu.device().limits().max_texture_dimension_2d,
        ) {
            Ok(Some(packed)) => {
                engine_scene::apply_packed_sprite_uvs(world, Some(&packed.layout));
                self.sprite_layout = Some(packed.layout.clone());
                let texture = gpu.create_texture_from_rgba(
                    "editor sprites",
                    packed.width,
                    packed.height,
                    &packed.rgba,
                );
                self.sprite_atlas = Some(gpu.create_sprite_atlas_binding(
                    &self.sprite_pipeline,
                    &TextureAtlas::new(texture, packed.layout),
                ));
            }
            Ok(None) => {
                engine_scene::apply_packed_sprite_uvs(world, None);
                self.sprite_atlas = None;
                self.sprite_layout = None;
            }
            Err(error) => {
                tracing::warn!(%error, "could not build scene sprite atlas");
                engine_scene::apply_packed_sprite_uvs(world, None);
                self.sprite_atlas = None;
                self.sprite_layout = None;
            }
        }
    }

    /// Drops live mesh renderers whose source ids changed so the next
    /// resolution pass uploads fresh GPU resources. Handles are released by
    /// removing the component, preserving ref-count ownership.
    pub fn invalidate_changed_assets(
        &mut self,
        world: &mut World,
        changed_ids: &std::collections::HashSet<engine_asset::AssetId>,
    ) -> usize {
        let stale: Vec<Entity> = world
            .query::<(Entity, &engine_ecs::components::MeshSource)>()
            .iter(world)
            .filter(|(_, source)| {
                crate::assets::parse_asset_id(&source.mesh)
                    .is_some_and(|id| changed_ids.contains(&id))
            })
            .map(|(entity, _)| entity)
            .collect();
        for entity in &stale {
            if let Ok(mut entity_mut) = world.get_entity_mut(*entity) {
                entity_mut.remove::<engine_ecs::components::MeshRenderer>();
            }
        }
        stale.len()
    }

    /// Drops every live mesh renderer before a dependency-blind hot reload.
    /// The next resolution pass recreates them from freshly decoded assets.
    pub fn invalidate_all_meshes(&mut self, world: &mut World) -> usize {
        let stale: Vec<Entity> = world
            .query::<(Entity, &engine_ecs::components::MeshSource)>()
            .iter(world)
            .map(|(entity, _)| entity)
            .collect();
        for entity in &stale {
            if let Ok(mut entity_mut) = world.get_entity_mut(*entity) {
                entity_mut.remove::<engine_ecs::components::MeshRenderer>();
            }
        }
        stale.len()
    }

    /// Renders `world` into this viewport's texture, with the gizmo drawn
    /// at `gizmo_origin` and a wireframe box for every visible entity
    /// that has no resolvable mesh.
    ///
    /// Errors are logged rather than returned: a dropped frame must not
    /// take the editor down, and the next frame usually recovers.
    pub fn render(
        &mut self,
        gpu: &GpuContext,
        world: &mut World,
        overlays: crate::OverlayToggles,
        sun: Option<crate::overlays::SunArrow>,
        selection: &[Entity],
    ) {
        self.update_water(gpu, world);
        self.lights = editor_lights(world);
        let sun_direction = self
            .lights
            .directional
            .first()
            .map(|sun| sun.direction)
            .unwrap_or(Vec3::NEG_Y);
        gpu.update_shadow_map(
            &mut self.shadow_map,
            directional_light_view_projection(sun_direction, self.camera.target, 40.0),
        );
        gpu.write_uniform_buffer(&self.camera_binding.buffer, &self.camera.to_uniform());
        gpu.write_uniform_buffer(
            &self.lights_binding.buffer,
            &self.lights.to_uniform_for_view(self.camera.eye),
        );
        let (sun_direction, sun_color) = self
            .lights
            .directional
            .first()
            .map(|light| (light.direction, light.color))
            .unwrap_or((Vec3::NEG_Y, Vec3::ONE));
        gpu.write_uniform_buffer(
            &self.skybox.buffer,
            &skybox_uniform(&self.camera, sun_direction, sun_color),
        );

        let placeholders = placeholder_positions(world);
        // One buffer for the frame's whole line list: the gizmo, the
        // placeholder boxes and every enabled overlay go in together, so
        // the cost of turning an overlay on is its vertices and not a
        // draw call.
        self.debug_draw.clear();
        for vertex in overlay_lines(&placeholders) {
            self.debug_draw.push_vertex(vertex);
        }
        let assets = &self.assets;
        crate::overlays::build(
            world,
            &self.lights,
            overlays,
            sun,
            selection,
            |world, entity| {
                let renderer = world.get::<engine_ecs::components::MeshRenderer>(entity)?;
                assets
                    .meshes
                    .get(renderer.mesh)
                    .map(|mesh| mesh.local_bounds)
            },
            &mut self.debug_draw,
        );
        let lines = self.debug_draw.vertices();
        let debug_lines = (!lines.is_empty()).then_some((&self.debug_line_pipeline, lines));

        let frustum = self.camera.frustum();
        let water_draws: Vec<_> = self
            .water_bindings
            .values()
            .map(|binding| WaterDraw { binding })
            .collect();
        let water = (!water_draws.is_empty()).then_some(WaterFrame {
            pipeline: &self.water_pipeline,
            draws: &water_draws,
        });
        if let Err(err) = engine_ecs::extract_and_render_to(
            Some(RenderTarget::new(&self.view)),
            world,
            &self.assets,
            gpu,
            &self.pipeline,
            &self.shadow_pipeline,
            &self.shadow_map,
            &self.skybox_pipeline,
            &self.skybox,
            &self.hdr_target,
            &self.post,
            &self.camera_binding,
            &frustum,
            &self.lights_binding,
            &self.skinned_pipeline,
            &self.instanced_pipeline,
            debug_lines,
            self.sprite_atlas.as_ref().map(|atlas| SpriteTarget {
                pipeline: &self.sprite_pipeline,
                quad: &self.sprite_quad,
                atlas,
            }),
            None,
            None,
            water,
            // The editor draws its own chrome through egui; the runtime
            // UI pass is for shipped games.
            None,
        ) {
            tracing::error!(error = %err, "editor viewport frame failed");
        }
    }

    fn update_water(&mut self, gpu: &GpuContext, world: &mut World) {
        let surfaces = water_surfaces(world);
        self.water_bindings
            .retain(|entity, _| surfaces.iter().any(|(live, _)| live == entity));
        for (entity, surface) in surfaces {
            let center = Vec3::new(surface.center.x, surface.base_height, surface.center.z);
            let uniform = WaterGpuUniform::new(
                center,
                surface.half_extent,
                surface.wave.direction,
                surface.wave.amplitude,
                surface.wave.length,
                surface.wave.speed,
                0.0,
            );
            let binding = self.water_bindings.entry(entity).or_insert_with(|| {
                gpu.create_water_binding(&self.water_pipeline, &self.pipeline, &uniform)
            });
            gpu.write_water_binding(binding, &uniform, &[]);
            let model = Mat4::from_scale_rotation_translation(
                Vec3::new(surface.half_extent.x, 1.0, surface.half_extent.y),
                Quat::IDENTITY,
                center,
            );
            gpu.write_uniform_buffer(
                &binding.model.buffer,
                &engine_renderer::ModelUniform {
                    model: model.to_cols_array_2d(),
                },
            );
        }
    }
}

fn water_surfaces(world: &mut World) -> Vec<(Entity, engine_physics::WaterSurface)> {
    world
        .query::<(Entity, &engine_ecs::components::WaterVolume)>()
        .iter(world)
        .map(|(entity, volume)| (entity, volume.surface))
        .collect()
}

/// World-space positions of every visible entity that names a model
/// ([`MeshSource`]) but has no [`MeshRenderer`]: its asset is missing or
/// failed to load. These get a red box so the problem is visible. Objects
/// that never had a shape (empties, lights, cameras, sounds) get nothing
/// here; cameras, lights and sounds are shown as icons instead.
fn placeholder_positions(world: &mut World) -> Vec<Vec3> {
    let mut query = world.query_filtered::<&GlobalTransform, (
        bevy_ecs::prelude::With<engine_ecs::components::MeshSource>,
        bevy_ecs::prelude::Without<MeshRenderer>,
        bevy_ecs::prelude::Without<Sprite>,
        bevy_ecs::prelude::Without<Disabled>,
    )>();
    query
        .iter(world)
        .map(|global| global.0.translation)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_sprite_does_not_get_missing_mesh_placeholder() {
        let mut world = World::new();
        world.spawn((
            GlobalTransform(engine_utils::Transform::from_translation(Vec3::ZERO)),
            engine_ecs::components::Sprite::new(
                glam::Vec2::ONE,
                engine_renderer::UvRect {
                    min: [0.0; 2],
                    max: [1.0; 2],
                },
            ),
        ));
        assert!(placeholder_positions(&mut world).is_empty());
    }

    #[test]
    fn only_unloaded_models_get_a_placeholder_box() {
        let mut world = World::new();
        let at = |x: f32| {
            GlobalTransform(engine_utils::Transform::from_translation(Vec3::new(
                x, 0.0, 0.0,
            )))
        };
        world.spawn(at(1.0)); // An empty or a sound: no box.
        world.spawn((
            at(2.0),
            engine_ecs::components::MeshSource {
                mesh: uuid::Uuid::new_v4().to_string(),
                material: uuid::Uuid::new_v4().to_string(),
            },
        ));
        assert_eq!(
            placeholder_positions(&mut world),
            vec![Vec3::new(2.0, 0.0, 0.0)]
        );
    }

    #[test]
    fn scene_view_lights_include_authored_point_light() {
        let mut world = World::new();
        world.spawn((
            engine_ecs::components::Transform::from(engine_utils::Transform::from_translation(
                Vec3::new(2.0, 3.0, 4.0),
            )),
            engine_scene::ScenePointLight(engine_scene::PointLightData::default()),
        ));
        let lights = super::editor_lights(&mut world);
        assert_eq!(lights.point.len(), 1);
        assert_eq!(lights.point[0].position, Vec3::new(2.0, 3.0, 4.0));
    }

    #[test]
    fn water_surface_collection_tracks_live_scene_entities() {
        let mut world = World::new();
        let entity = world
            .spawn(engine_ecs::components::WaterVolume {
                surface: engine_physics::WaterSurface::default(),
            })
            .id();
        assert_eq!(water_surfaces(&mut world).len(), 1);
        world.despawn(entity);
        assert!(water_surfaces(&mut world).is_empty());
    }

    #[test]
    fn deleted_instanced_group_is_identified_without_releasing_survivors() {
        use engine_ecs::components::InstancedMeshRenderer;
        use engine_utils::{AssetHandle, AssetId};

        let mut world = World::new();
        let first_mesh = AssetHandle::new(AssetId::new());
        let first_material = AssetHandle::new(AssetId::new());
        let second_mesh = AssetHandle::new(AssetId::new());
        let second_material = AssetHandle::new(AssetId::new());
        let deleted = world
            .spawn(InstancedMeshRenderer {
                mesh: first_mesh,
                material: first_material,
                instances: Vec::new(),
            })
            .id();
        world.spawn(InstancedMeshRenderer {
            mesh: second_mesh,
            material: second_material,
            instances: Vec::new(),
        });
        let before = super::instanced_handles(&mut world);
        world.despawn(deleted);

        assert_eq!(
            super::removed_instanced_handles(&before, &super::instanced_handles(&mut world)),
            vec![(first_mesh, first_material)]
        );
    }

    #[test]
    fn box_has_twelve_edges_as_twentyfour_vertices() {
        let mut lines = Vec::new();
        push_wire_box(&mut lines, Vec3::ZERO, 1.0, PLACEHOLDER_COLOR);
        assert_eq!(lines.len(), 24, "12 edges, 2 vertices each");
    }

    #[test]
    fn box_corners_are_symmetric_about_the_centre() {
        let corners = box_corners(Vec3::new(5.0, 0.0, 0.0), 2.0);
        let sum: Vec3 = corners.iter().copied().sum();
        // Eight corners symmetric about the centre average back to it.
        assert!((sum / 8.0 - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn overlay_has_no_lines_with_nothing_to_draw() {
        assert!(
            overlay_lines(&[]).is_empty(),
            "an empty overlay must skip the draw"
        );
    }

    #[test]
    fn overlay_includes_a_box_per_placeholder() {
        let lines = overlay_lines(&[Vec3::ZERO, Vec3::X, Vec3::Y]);
        assert_eq!(lines.len(), 3 * 24);
    }
}
