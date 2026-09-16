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

use engine_ecs::components::{Disabled, GlobalTransform, MeshRenderer};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{
    Camera, CameraBinding, DebugLinePipeline, DebugLineVertex, DirectionalLight, GpuContext,
    HdrTarget, InstancedPipeline, LightSet, LightsBinding, Pipeline, PostProcessStack,
    PostSettings, RenderAssets, RenderTarget, ShadowMap, ShadowPipeline, SkinnedPipeline,
    SkyboxBinding, SkyboxPipeline, directional_light_view_projection, skybox_uniform,
};
use glam::{Mat4, Vec3};

use crate::error::EditorError;
use crate::gizmo::{self, GizmoMode};
use crate::shell::EditorShell;

/// `egui_wgpu::Renderer::register_native_texture` requires exactly this
/// format for a texture it's asked to display.
pub const VIEWPORT_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Half-extent of the wireframe box drawn for an entity with no
/// resolvable mesh, in world units. Big enough to click, small enough not
/// to swamp real geometry.
const PLACEHOLDER_HALF_EXTENT: f32 = 0.5;

/// Colour of that wireframe box: a desaturated amber, readable as
/// "something is here but its asset didn't load".
const PLACEHOLDER_COLOR: [f32; 4] = [0.85, 0.65, 0.2, 1.0];

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

/// Builds the frame's overlay line list: a wireframe box per entity with
/// no resolvable mesh, plus the gizmo handles at `gizmo_origin`.
///
/// Pure over its inputs so it can be unit-tested without a GPU.
fn overlay_lines(
    placeholders: &[Vec3],
    gizmo_origin: Option<Vec3>,
    gizmo_mode: GizmoMode,
    two_d: bool,
) -> Vec<DebugLineVertex> {
    let mut lines = Vec::new();
    for &centre in placeholders {
        push_wire_box(
            &mut lines,
            centre,
            PLACEHOLDER_HALF_EXTENT,
            PLACEHOLDER_COLOR,
        );
    }
    if let Some(origin) = gizmo_origin {
        for &axis in gizmo::axes_for(gizmo_mode, two_d) {
            let (start, end) = gizmo::axis_endpoints(origin, axis);
            let color = axis.color();
            lines.push(DebugLineVertex {
                position: start.into(),
                color,
            });
            lines.push(DebugLineVertex {
                position: end.into(),
                color,
            });
        }
    }
    lines
}

/// The editor's off-screen scene render.
///
/// Fixed size rather than resizing to fill its panel — resizing means
/// recreating the texture, the HDR target and the post stack, and
/// re-registering with egui, which is future work.
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

        let texture = device.create_texture(&wgpu::TextureDescriptor {
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let texture_id = shell.register_texture(device, &view);

        let pipeline = gpu.create_pbr_pipeline("editor pbr");
        let shadow_pipeline = gpu.create_shadow_pipeline(&pipeline, "editor shadow");
        let skinned_pipeline = gpu.create_skinned_pbr_pipeline(&pipeline, "editor skinned pbr");
        let instanced_pipeline =
            gpu.create_instanced_pbr_pipeline(&pipeline, "editor instanced pbr");
        let skybox_pipeline = gpu.create_skybox_pipeline("editor skybox");
        let debug_line_pipeline = gpu.create_debug_line_pipeline(&pipeline, "editor overlay lines");

        let camera = Self::perspective_camera(width, height);
        // One sun, angled down and slightly forward so authored geometry
        // reads as solid rather than flat-lit.
        let lights = LightSet {
            directional: vec![DirectionalLight {
                direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
                color: Vec3::ONE,
                intensity: 3.0,
            }],
            point: Vec::new(),
            // Same daylight ambient the runtime defaults to, so the
            // Scene view and a shipped build agree on how things look.
            ambient: engine_renderer::AmbientLight::DAYLIGHT,
        };
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
        })
    }

    /// The perspective camera used in 3D authoring mode.
    fn perspective_camera(width: u32, height: u32) -> Camera {
        Camera::new(
            Vec3::new(4.0, 3.0, 6.0),
            Vec3::ZERO,
            width as f32 / height.max(1) as f32,
        )
    }

    /// The orthographic front view used in 2D authoring mode.
    fn orthographic_camera(width: u32, height: u32) -> Camera {
        Camera::new_orthographic(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::ZERO,
            width as f32 / height.max(1) as f32,
            10.0,
        )
    }

    /// Switches between the 3D perspective camera and the 2D
    /// orthographic front view, re-uploading only when the mode actually
    /// changes.
    pub fn set_dimension(&mut self, gpu: &GpuContext, two_d: bool) {
        if self.two_d == two_d {
            return;
        }
        self.two_d = two_d;
        self.camera = if two_d {
            Self::orthographic_camera(self.width, self.height)
        } else {
            Self::perspective_camera(self.width, self.height)
        };
        gpu.write_uniform_buffer(&self.camera_binding.buffer, &self.camera.to_uniform());
    }

    /// This viewport's pixel dimensions.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
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
    }

    /// How many meshes this viewport currently holds — the hook a test
    /// uses to prove a project swap actually released them.
    pub fn uploaded_mesh_count(&self) -> usize {
        self.assets.meshes.len()
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
        crate::resolve_pending(world, importer, gpu, &self.pipeline, &mut self.assets)
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
        gizmo_mode: GizmoMode,
        gizmo_origin: Option<Vec3>,
        overlays: crate::OverlayToggles,
    ) {
        gpu.write_uniform_buffer(&self.camera_binding.buffer, &self.camera.to_uniform());
        gpu.write_uniform_buffer(&self.lights_binding.buffer, &self.lights.to_uniform());
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
        for vertex in overlay_lines(&placeholders, gizmo_origin, gizmo_mode, self.two_d) {
            self.debug_draw.push_vertex(vertex);
        }
        crate::overlays::build(world, &self.lights, overlays, &mut self.debug_draw);
        let lines = self.debug_draw.vertices();
        let debug_lines = (!lines.is_empty()).then_some((&self.debug_line_pipeline, lines));

        let frustum = self.camera.frustum();
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
            // No sprites: every sprite in a scene samples its own atlas
            // texture, and the Scene view has no atlas binding to draw
            // them with yet. 2D scenes show their meshes, not their
            // sprites, until that is wired.
            None,
            None,
            None,
            // The editor draws its own chrome through egui; the runtime
            // UI pass is for shipped games.
            None,
        ) {
            tracing::error!(error = %err, "editor viewport frame failed");
        }
    }
}

/// World-space positions of every visible entity that has a transform but
/// no [`MeshRenderer`] — lights, cameras, empties, and anything whose
/// asset failed to resolve. These get a wireframe box so they stay
/// visible and selectable.
fn placeholder_positions(world: &mut World) -> Vec<Vec3> {
    let mut query = world.query_filtered::<&GlobalTransform, (
        bevy_ecs::prelude::Without<MeshRenderer>,
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
        let lines = overlay_lines(&[], None, GizmoMode::Translate, false);
        assert!(lines.is_empty(), "an empty overlay must skip the draw");
    }

    #[test]
    fn overlay_includes_a_box_per_placeholder() {
        let lines = overlay_lines(
            &[Vec3::ZERO, Vec3::X, Vec3::Y],
            None,
            GizmoMode::Translate,
            false,
        );
        assert_eq!(lines.len(), 3 * 24);
    }

    #[test]
    fn overlay_appends_gizmo_handles_after_placeholders() {
        let with_gizmo =
            overlay_lines(&[Vec3::ZERO], Some(Vec3::ZERO), GizmoMode::Translate, false);
        let axes = gizmo::axes_for(GizmoMode::Translate, false).len();
        assert_eq!(with_gizmo.len(), 24 + axes * 2);
    }

    #[test]
    fn two_d_mode_draws_fewer_gizmo_axes_than_three_d() {
        let three_d = overlay_lines(&[], Some(Vec3::ZERO), GizmoMode::Translate, false);
        let two_d = overlay_lines(&[], Some(Vec3::ZERO), GizmoMode::Translate, true);
        assert!(two_d.len() < three_d.len());
    }
}
