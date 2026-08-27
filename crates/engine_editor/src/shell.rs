//! The editor's egui integration shell: owns the egui context, the
//! `egui-winit` event bridge, and the `egui-wgpu` renderer that draws
//! egui's output.

use egui::{Context as EguiContext, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use egui_winit::State as EguiWinitState;
use engine_platform::{Window, WindowEvent};
use glam::Vec2;

use crate::assets;
use crate::console;
use crate::gizmo::{self, Axis};
use crate::hierarchy;
use crate::inspector;
use crate::state::EditorState;
use crate::viewport::Viewport;

/// Owns everything needed to run and draw an egui-based UI into a wgpu
/// surface: the egui context (retained UI state), the winit event bridge
/// ([`egui_winit::State`]), and the wgpu renderer ([`egui_wgpu::Renderer`]).
///
/// This is the editor's "shell" — the foundation every panel (hierarchy,
/// inspector, scene view, asset browser, console, gizmos) is built
/// inside. See [`EditorShell::run_frame`] for what it currently draws.
pub struct EditorShell {
    context: EguiContext,
    winit_state: EguiWinitState,
    renderer: EguiRenderer,
}

impl EditorShell {
    /// Sets up the shell against `window` (for the winit event bridge)
    /// and `device`/`surface_format` (for the wgpu renderer) —
    /// `surface_format` should match whatever the target surface is
    /// actually configured with (e.g. an existing
    /// `engine_renderer::GpuContext`'s `.config().format`, so the editor
    /// draws through the same device/surface the rest of the engine
    /// already uses, rather than standing up a second one).
    pub fn new(
        window: &Window,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let context = EguiContext::default();
        let winit_state =
            EguiWinitState::new(context.clone(), ViewportId::ROOT, window, None, None, None);
        let renderer = EguiRenderer::new(device, surface_format, RendererOptions::default());
        Self {
            context,
            winit_state,
            renderer,
        }
    }

    /// Feeds a raw winit event to egui — call this from
    /// [`engine_platform::PlatformHandler::on_raw_window_event`].
    ///
    /// Returns whether egui consumed the event. Callers driving other
    /// input-reactive systems (camera controls, gameplay input, ...)
    /// alongside the editor should skip their own handling when this is
    /// `true` — e.g. a click that landed on an egui panel shouldn't also
    /// register as a click in a 3D viewport underneath it.
    pub fn handle_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        self.winit_state.on_window_event(window, event).consumed
    }

    /// Registers `view` as an egui-displayable texture — `view` must be
    /// [`wgpu::TextureFormat::Rgba8Unorm`], the same requirement
    /// [`egui_wgpu::Renderer::register_native_texture`] has, since this
    /// just forwards to it. Used by [`Viewport::new`] to make its
    /// off-screen render target showable in a panel.
    pub fn register_texture(
        &mut self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> TextureId {
        self.renderer
            .register_native_texture(device, view, wgpu::FilterMode::Linear)
    }

    /// Runs one frame of UI, returning the output ready for
    /// [`EditorShell::render`].
    ///
    /// Draws a top bar, a left "Hierarchy" panel listing `state.world`'s
    /// entities (see [`hierarchy::show`] — clicking one updates
    /// `state.selected_entity`), a right "Inspector" panel editing
    /// whatever's currently selected (see [`inspector::show`]), a bottom
    /// "Assets" panel listing `state.assets` (see `assets::show`), a
    /// "Console" panel showing `state.console`'s captured log lines (see
    /// `console::show`), and a "Scene View" panel showing `viewport`'s
    /// rendered content (call [`Viewport::render`] beforehand so the
    /// image reflects this frame, not a stale one). Gizmos are a
    /// separate, later iteration that will add one more panel alongside
    /// these.
    pub fn run_frame(
        &mut self,
        window: &Window,
        viewport: &Viewport,
        state: &mut EditorState,
    ) -> FullOutput {
        let raw_input = self.winit_state.take_egui_input(window);

        let output = self.context.run_ui(raw_input, |ui| {
            egui::Panel::top("vge_editor_top_panel").show(ui, |ui| {
                ui.heading("VGE Editor");
            });
            egui::Panel::left("vge_editor_hierarchy_panel").show(ui, |ui| {
                ui.heading("Hierarchy");
                ui.separator();
                hierarchy::show(ui, &mut state.world, &mut state.selected_entity);
            });
            egui::Panel::right("vge_editor_inspector_panel").show(ui, |ui| {
                ui.heading("Inspector");
                ui.separator();
                inspector::show(ui, &mut state.world, state.selected_entity);
            });
            egui::Panel::bottom("vge_editor_console_panel").show(ui, |ui| {
                ui.heading("Console");
                ui.separator();
                console::show(ui, &state.console);
            });
            egui::Panel::bottom("vge_editor_assets_panel").show(ui, |ui| {
                ui.heading("Assets");
                ui.separator();
                assets::show(ui, &state.assets, &mut state.selected_asset);
            });
            egui::CentralPanel::default().show(ui, |ui| {
                ui.label("Scene View");
                let (width, height) = viewport.size();
                let image_response = ui.add(
                    egui::Image::new((
                        viewport.texture_id(),
                        egui::vec2(width as f32, height as f32),
                    ))
                    .sense(egui::Sense::click_and_drag()),
                );
                handle_gizmo_interaction(viewport, state, &image_response);
            });
        });

        self.winit_state
            .handle_platform_output(window, output.platform_output.clone());
        output
    }

    /// Applies `full_output`'s pending texture updates/frees (e.g. the
    /// font atlas) to the GPU, then clears them.
    ///
    /// Needs only `device`/`queue`, not a live frame or command encoder —
    /// call this **unconditionally** every frame, straight after
    /// [`EditorShell::run_frame`], even on a frame that ends up not being
    /// drawn at all (e.g. `engine_renderer::GpuContext::render_with` couldn't acquire a
    /// surface texture — occluded window, first-frame timing, ...).
    ///
    /// This matters because [`egui::FullOutput`]'s `textures_delta` field
    /// panics (in debug builds) if dropped without being drained — egui
    /// assumes every delta it emits gets applied and won't re-emit one
    /// that's silently discarded, so texture handling can't live inside
    /// the same "might not run this frame" path as [`EditorShell::render`]
    /// without risking exactly that crash (this is why the two are
    /// separate methods, not one).
    pub fn apply_texture_updates(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        full_output: &mut FullOutput,
    ) {
        for (id, image_deltas) in &full_output.textures_delta.set {
            for image_delta in image_deltas {
                self.renderer
                    .update_texture(device, queue, *id, image_delta);
            }
        }
        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        full_output.textures_delta.clear();
    }

    /// Draws `full_output`'s shapes into `view`, recording draw commands
    /// into `encoder`. `screen_descriptor` must reflect the surface's
    /// current physical size and scale factor.
    ///
    /// Call [`EditorShell::apply_texture_updates`] first — this method
    /// doesn't touch `full_output.textures_delta` at all.
    ///
    /// Clears `view` to a neutral dark background first — this draws
    /// egui's output (including the "Scene View" panel's embedded
    /// [`Viewport`] image) over the whole window; the 3D content itself
    /// was already rendered separately into the viewport's own texture
    /// (see [`Viewport::render`]), not into `view`.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen_descriptor: &ScreenDescriptor,
        full_output: FullOutput,
    ) {
        let clipped_primitives = self
            .context
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.renderer.update_buffers(
            device,
            queue,
            encoder,
            &clipped_primitives,
            screen_descriptor,
        );

        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE editor egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.1,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // `egui-wgpu`'s `render` needs a `'static`-lifetime pass so it
            // can be shared with user paint callbacks; this shell has none,
            // but the API requires it regardless.
            let mut render_pass = render_pass.forget_lifetime();
            self.renderer
                .render(&mut render_pass, &clipped_primitives, screen_descriptor);
        }
    }
}

/// Drives the translate gizmo's click-and-drag interaction for the Scene
/// View panel: picks an axis when a drag starts on one of its handles,
/// moves the selected entity's `Transform` along that axis while
/// dragging, and clears the drag on release.
///
/// A free function (not an `EditorShell` method) — it only needs
/// `viewport`/`state`/`image_response`, not the shell's egui context or
/// renderer.
fn handle_gizmo_interaction(
    viewport: &Viewport,
    state: &mut EditorState,
    image_response: &egui::Response,
) {
    let (width, height) = viewport.size();
    let screen_size = Vec2::new(width as f32, height as f32);

    let selected_entity = state.selected_entity;
    let Some(origin) = state.gizmo_origin() else {
        state.dragging_axis = None;
        return;
    };

    let view_proj = viewport.view_projection_matrix();
    let handles: Vec<(Axis, Vec2, Vec2)> = Axis::ALL
        .into_iter()
        .filter_map(|axis| {
            let (start, end) = gizmo::axis_endpoints(origin, axis);
            let start_screen = gizmo::project_to_screen(view_proj, start, screen_size)?;
            let end_screen = gizmo::project_to_screen(view_proj, end, screen_size)?;
            Some((axis, start_screen, end_screen))
        })
        .collect();

    if image_response.drag_started()
        && let Some(pointer) = image_response.interact_pointer_pos()
    {
        let local = Vec2::new(
            pointer.x - image_response.rect.min.x,
            pointer.y - image_response.rect.min.y,
        );
        state.dragging_axis = gizmo::pick_axis(&handles, local);
    }

    if image_response.dragged()
        && let Some(axis) = state.dragging_axis
        && let Some(&(_, start_screen, end_screen)) = handles
            .iter()
            .find(|&&(handle_axis, _, _)| handle_axis == axis)
    {
        let pixel_length = (end_screen - start_screen).length();
        if pixel_length > f32::EPSILON
            && let Some(entity) = selected_entity
            && let Some(mut transform) = inspector::transform_of(&state.world, entity)
        {
            let delta = image_response.drag_delta();
            let delta_along_axis =
                gizmo::drag_delta_along_axis(start_screen, end_screen, Vec2::new(delta.x, delta.y));
            let world_per_pixel = gizmo::HANDLE_LENGTH / pixel_length;
            transform.translation += axis.direction() * delta_along_axis * world_per_pixel;
            inspector::set_transform(&mut state.world, entity, transform);
        }
    }

    if image_response.drag_stopped() {
        state.dragging_axis = None;
    }
}
