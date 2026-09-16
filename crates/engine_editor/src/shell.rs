//! The editor's egui integration shell: owns the egui context, the
//! `egui-winit` event bridge, and the `egui-wgpu` renderer that draws
//! egui's output.

use egui::{Context as EguiContext, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use egui_winit::State as EguiWinitState;
use engine_platform::{Window, WindowEvent};
use glam::{Vec2, Vec3};

use crate::assets;
use crate::chrome::{BottomTab, InspectorTab, TransformSpace, Workspace};
use crate::console;
use crate::diagnostics;
use crate::dirty::PendingAction;
#[cfg(feature = "dock-shell")]
use crate::dock;
use crate::gizmo::{self, Axis, GizmoMode};
use crate::hierarchy;
use crate::inspector;
use crate::output;
use crate::project_swap::ProjectRequest;
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
    #[cfg(feature = "dock-shell")]
    dock: dock::Host,
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
        crate::theme::install(&context);
        let winit_state =
            EguiWinitState::new(context.clone(), ViewportId::ROOT, window, None, None, None);
        let renderer = EguiRenderer::new(device, surface_format, RendererOptions::default());
        Self {
            context,
            winit_state,
            renderer,
            #[cfg(feature = "dock-shell")]
            dock: dock::Host::new(),
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
    /// Draws the RustyEngine Studio shell: a menu bar, a toolbar
    /// (play/pause/stop, workspace switcher, gizmo mode, transform space,
    /// build config), a status bar, and the dockable panels — Hierarchy,
    /// Scene, Asset Browser, Code, Console/Problems/Output, and
    /// Inspector/Lighting — each shown only when `state.panels` says so.
    /// The central Scene panel shows `viewport`'s rendered content (call
    /// [`Viewport::render`] beforehand so the image reflects this frame,
    /// not a stale one) with the translate/rotate/scale gizmo drawn over
    /// the selection.
    ///
    /// The AI Assistant and Profiler panels exist in the codebase but are
    /// not wired into the shell yet; the Code panel's contents are a
    /// placeholder that the Monaco webview floats over.
    pub fn run_frame(
        &mut self,
        window: &Window,
        viewport: &Viewport,
        state: &mut EditorState,
    ) -> FullOutput {
        let raw_input = self.winit_state.take_egui_input(window);

        // Snapshot the pre-edit world once per settled edit sequence, so
        // whatever the panels below mutate can be undone as one step.
        // Skipped in Play mode — the running sim mutates the world every
        // frame, and Stop rewinds it wholesale anyway.
        if !state.play.is_playing() {
            state
                .history
                .begin_frame(&mut state.world, state.selected_entity);
        }

        let output = self.context.run_ui(raw_input, |ui| {
            egui::Panel::top("studio_menu_bar").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    menu_file(ui, state);
                    menu_edit(ui, state);
                    menu_assets(ui, state);
                    menu_gameobject(ui, state);
                    menu_component(ui);
                    menu_view(ui, state);
                    menu_tools(ui);
                    menu_window(ui, state);
                    menu_help(ui);
                });
            });
            egui::Panel::top("studio_toolbar").show(ui, |ui| {
                toolbar(ui, state);
            });
            egui::Panel::bottom("studio_status_bar")
                .resizable(false)
                .exact_size(22.0)
                .show(ui, |ui| {
                    status_bar(ui, state);
                });
            #[cfg(feature = "dock-shell")]
            {
                state.code_panel_rect = None;
                state.code_panel_visible = false;
                let mut viewer = StudioDockViewer { state, viewport };
                self.dock.show(ui, &mut viewer);
            }

            #[cfg(not(feature = "dock-shell"))]
            {
            // Panel claim order builds the reference layout (see
            // `Dev Documents/prototype.png`):
            //
            //   [ Hierarchy   | Scene [tabs] | Inspector ]
            //   [ ─────────── | ──────────── |           ]
            //   [ Asset Brow. | Code [tabs]  |           ]
            //   [ Console / Problems / Output  (full width) ]
            //
            // The Console is claimed *before* the side docks so it spans
            // the whole width; the Code panel is claimed *after* them so
            // it insets to the centre column. Every panel is
            // `show_collapsible` — drag its resize handle to the edge and
            // it slides shut (VSCode-style); the Window-menu checkbox
            // toggles the same `panels.*` flag. `panels.*` is copied into
            // a local `open` first so the drag-collapse `&mut bool`
            // doesn't alias the `state` a panel body also borrows.

            let mut open = state.panels.console;
            egui::Panel::bottom("studio_console_panel")
                .resizable(true)
                .default_size(170.0)
                .show_collapsible(ui, &mut open, |ui| bottom_tabs_panel(ui, state));
            state.panels.console = open;

            // CPU frame profiler — a thin bottom strip below the Console,
            // shown only when toggled on (Window menu) or by the Debug
            // workspace preset.
            let mut profiler_open = state.panels.profiler;
            egui::Panel::bottom("studio_profiler_panel")
                .resizable(true)
                .default_size(120.0)
                .show_collapsible(ui, &mut profiler_open, |ui| {
                    ui.heading("Profiler");
                    ui.separator();
                    crate::profiler::show(ui, &state.profiler);
                });
            state.panels.profiler = profiler_open;

            // Left dock: Hierarchy filling the column, Asset Browser
            // pinned to its lower half. Collapsing the dock (its own
            // handle, or the Hierarchy checkbox) takes the Asset Browser
            // with it.
            let mut left_open = state.panels.hierarchy;
            egui::Panel::left("studio_left_dock")
                .resizable(true)
                .default_size(240.0)
                .min_size(170.0)
                .show_collapsible(ui, &mut left_open, |ui| {
                    let mut ab_open = state.panels.asset_browser;
                    egui::Panel::bottom("studio_asset_panel")
                        .resizable(true)
                        // Order matters: `default_size` widens the size
                        // range, so the caps come after it.
                        .default_size(240.0)
                        .min_size(assets::BROWSER_MIN_HEIGHT)
                        .max_size(assets::browser_panel_max(ui.available_height()))
                        .show_collapsible(ui, &mut ab_open, |ui| {
                            ui.heading("Asset Browser");
                            ui.separator();
                            // The preview claims a fixed strip at the
                            // bottom *before* the file list is drawn, so
                            // the list's scroll area gets the space that
                            // is left instead of pushing the preview off
                            // the panel. Both are bounded: a panel that
                            // sizes itself to its contents is a panel
                            // that eats the dock.
                            if state.asset_browser.preview_open {
                                let selected_entry = state.selected_asset.as_ref().and_then(|path| {
                                    state
                                        .assets
                                        .iter()
                                        .find(|entry| &entry.relative_path == path)
                                });
                                let (preview_max, preview_default) =
                                    assets::preview_strip_size(ui.available_height());
                                egui::Panel::bottom("studio_asset_preview")
                                    .resizable(true)
                                    .default_size(preview_default)
                                    .min_size(assets::PREVIEW_MIN_HEIGHT)
                                    .max_size(preview_max)
                                    .show_collapsible(
                                        ui,
                                        &mut state.asset_browser.preview_open,
                                        |ui| {
                                        egui::ScrollArea::vertical()
                                            .auto_shrink([false, false])
                                            .min_scrolled_height(0.0)
                                            .show(ui, |ui| {
                                                crate::preview::show(
                                                    ui,
                                                    selected_entry,
                                                    &state.importer,
                                                    &mut state.preview,
                                                );
                                            });
                                        },
                                    );
                            }
                            assets::show(
                                ui,
                                &state.assets,
                                state.importer.stats(),
                                &mut state.asset_browser,
                                &mut state.selected_asset,
                                &mut state.file_open_request,
                            );
                        });
                    state.panels.asset_browser = ab_open;

                    ui.heading("Hierarchy");
                    ui.separator();
                    hierarchy::show(
                        ui,
                        &mut state.world,
                        &mut state.selected_entity,
                        &mut state.secondary_selection,
                        &mut state.hierarchy,
                    );
                    if let Some(entity) = state.hierarchy.prefab_request.take() {
                        state.create_prefab_request = Some(entity);
                    }
                });
            state.panels.hierarchy = left_open;

            let mut open = state.panels.inspector;
            egui::Panel::right("studio_inspector_panel")
                .resizable(true)
                .default_size(300.0)
                .show_collapsible(ui, &mut open, |ui| inspector_tabs_panel(ui, state));
            state.panels.inspector = open;

            // AI Assistant dock — a placeholder until the `editor_ai`
            // provider/agent track lands; the panel + its toggles are
            // wired now so the layout is stable.
            let mut ai_open = state.panels.ai;
            egui::Panel::right("studio_ai_panel")
                .resizable(true)
                .default_size(280.0)
                .show_collapsible(ui, &mut ai_open, |ui| {
                    ai_panel(ui, state);
                });
            state.panels.ai = ai_open;

            // Code editor: inset to the centre column, above the Console.
            // The panel keeps a thin egui header (a faux tab + a close
            // button); the Monaco webview floats over everything *below*
            // that header, so the panel's drag-resize handle and the
            // close button stay reachable — a native view on top would
            // otherwise swallow them. `code_panel_body` fills the panel
            // to its far edge so it holds `default_size` instead of
            // collapsing to the header height.
            let mut code_open = state.panels.code;
            let mut close_code = false;
            let code_shown = egui::Panel::bottom("studio_code_panel")
                .resizable(true)
                .min_size(140.0)
                .default_size(300.0)
                .show_collapsible(ui, &mut code_open, |ui| {
                    code_panel_body(ui, &mut close_code)
                });
            state.panels.code = code_open && !close_code;
            state.code_panel_rect = code_shown.map(|inner| {
                let r = inner.response.rect;
                let top = r.min.y + CODE_HEADER_HEIGHT;
                [r.min.x, top, r.width(), (r.max.y - top).max(1.0)]
            });
            // The Monaco webview floats over this rect; hidden when the
            // panel is collapsed or a modal is up (egui can't draw over a
            // native view).
            state.code_panel_visible =
                state.code_panel_rect.is_some() && state.pending_action.is_none();

            // Drag-resize detection: the rect moved this frame while a
            // pointer button is held. The host hides the webview for that
            // frame so the native view doesn't lag/overhang the edge.
            let pointer_down = ui.input(|i| i.pointer.any_down());
            state.code_panel_resizing = pointer_down
                && state.prev_code_panel_rect.is_some()
                && state.code_panel_rect != state.prev_code_panel_rect;
            state.prev_code_panel_rect = state.code_panel_rect;

            // Ctrl/Cmd+S with the Code panel up saves the active file
            // even when egui holds focus (when Monaco has focus it
            // handles the key itself and egui never sees it).
            if state.panels.code
                && ui.input(|i| {
                    (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::S)
                })
            {
                state.code_save_requested = true;
            }

            egui::CentralPanel::default().show(ui, |ui| {
                scene_view_tabs(ui);
                ui.separator();
                egui::CollapsingHeader::new("Terrain & Vegetation Tools")
                    .default_open(false)
                    .show(ui, |ui| crate::terrain_tools::show_controls(ui, &mut state.terrain_tools));
                let (width, height) = viewport.size();
                let image_response = ui
                    .add(
                    egui::Image::new((
                        viewport.texture_id(),
                        egui::vec2(width as f32, height as f32),
                    ))
                    .sense(egui::Sense::click_and_drag()),
                    )
                    // The scene is clickable for gizmos, but idle hovering
                    // is not a grab operation. Only an active drag should
                    // request a grabbing cursor.
                    .on_hover_cursor(egui::CursorIcon::Default);
                handle_gizmo_interaction(viewport, state, &image_response);

                // An asset row dragged from the Asset Browser and dropped
                // here spawns an entity: a `.prefab` is instantiated from
                // its template, anything else gets a placeholder entity
                // referencing the file.
                if let Some(path) = image_response.dnd_release_payload::<std::path::PathBuf>() {
                    let is_prefab = path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("prefab"));
                    let spawned = if is_prefab {
                        match state.spawn_prefab(&path) {
                            Ok(entity) => Some(entity),
                            Err(err) => {
                                tracing::warn!(error = %err, path = %path.display(), "could not instantiate prefab");
                                None
                            }
                        }
                    } else {
                        Some(state.spawn_asset_entity(&path))
                    };
                    if let Some(entity) = spawned {
                        state.select_only(entity);
                        state.mark_dirty();
                    }
                }
                // This image is interactive for gizmo drags, but it is not
                // itself a movable surface. Keep the idle pointer neutral;
                // only an actual gizmo drag gets a grabbing cursor.
                ui.ctx().set_cursor_icon(scene_cursor_icon(
                    image_response.dragged(),
                    state.dragging_axis.is_some(),
                ));
            });
            }

            unsaved_changes_modal(ui, state);
        });

        // Ctrl/Cmd+Z undo, Ctrl/Cmd+Shift+Z or Ctrl/Cmd+Y redo.
        let (undo_pressed, redo_pressed) = self.context.input(|input| {
            let cmd = input.modifiers.command;
            let z = input.key_pressed(egui::Key::Z);
            let y = input.key_pressed(egui::Key::Y);
            (
                cmd && z && !input.modifiers.shift,
                cmd && ((z && input.modifiers.shift) || y),
            )
        });
        if undo_pressed {
            state
                .history
                .undo(&mut state.world, &mut state.selected_entity);
            state.secondary_selection.clear();
            state.dragging_axis = None;
        } else if redo_pressed {
            state
                .history
                .redo(&mut state.world, &mut state.selected_entity);
            state.secondary_selection.clear();
            state.dragging_axis = None;
        }

        // Commit the frame's edits as one undo step — but not while a
        // pointer drag or text entry is still in progress, so a
        // multi-frame drag coalesces into a single step.
        let pointer_down = self.context.input(|input| input.pointer.any_down());
        let widget_focused = self.context.memory(|memory| memory.focused().is_some());
        if !state.play.is_playing()
            && !pointer_down
            && !widget_focused
            && state
                .history
                .settle(&mut state.world, state.selected_entity)
        {
            state.dirty.mark_scene();
        }

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

/// Cursor contract for the Scene image. The image accepts clicks and drags
/// for gizmos, but must not advertise a grab operation while idle. Keeping
/// this decision in one pure helper makes the interaction regression-testable
/// without creating a GPU window in CI.
fn scene_cursor_icon(image_dragged: bool, gizmo_dragging: bool) -> egui::CursorIcon {
    if image_dragged && gizmo_dragging {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Default
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
    let handles: Vec<(Axis, Vec2, Vec2)> =
        gizmo::axes_for(state.gizmo_mode, state.dimension.is_2d())
            .iter()
            .filter_map(|&axis| {
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
        if pixel_length > f32::EPSILON && selected_entity.is_some() {
            let delta = image_response.drag_delta();
            let delta_along_axis =
                gizmo::drag_delta_along_axis(start_screen, end_screen, Vec2::new(delta.x, delta.y));
            // The same handle-drag delta is applied to every selected,
            // unlocked entity that has a `Transform` — the batch move.
            let targets: Vec<_> = state
                .selection()
                .into_iter()
                .filter(|&e| state.world.get::<engine_ecs::components::Lock>(e).is_none())
                .collect();
            for target in targets {
                let Some(transform) = inspector::transform_of(&state.world, target) else {
                    continue;
                };
                let updated = match state.gizmo_mode {
                    GizmoMode::Translate => {
                        // Pixels along the handle → world units, scaled so
                        // dragging the handle its full on-screen length
                        // moves the entity one `HANDLE_LENGTH`.
                        let world_per_pixel = gizmo::HANDLE_LENGTH / pixel_length;
                        gizmo::translate_transform(
                            transform,
                            axis,
                            delta_along_axis * world_per_pixel,
                        )
                    }
                    GizmoMode::Rotate => gizmo::rotate_transform(
                        transform,
                        axis,
                        delta_along_axis * gizmo::ROTATE_RADIANS_PER_PIXEL,
                    ),
                    GizmoMode::Scale => gizmo::scale_transform(
                        transform,
                        axis,
                        1.0 + delta_along_axis * gizmo::SCALE_PER_PIXEL,
                    ),
                };
                inspector::set_transform(&mut state.world, target, updated);
            }
        }
    }

    if image_response.drag_stopped() {
        state.dragging_axis = None;
    }
}

// --- Menu bar --------------------------------------------------------------
//
// Each `menu_*` free function draws one top-level menu. Items backed by a
// real action are live; the rest are disabled placeholders so the shell
// matches the canonical UI while the gaps stay visible.

/// File menu: project New / Open / Open Recent, scene New / Revert /
/// Save, and Exit. Everything that would discard work routes through
/// [`request_guarded`] so unsaved changes prompt first.
fn menu_file(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("File", |ui| {
        if ui.button("New Project…").clicked() {
            if let Some(root) = pick_project_directory("Create project in") {
                request_guarded(state, PendingAction::NewProject(root));
            }
            ui.close();
        }
        if ui.button("Open Project…").clicked() {
            if let Some(root) = pick_project_directory("Open project") {
                request_guarded(state, PendingAction::OpenProject(root));
            }
            ui.close();
        }
        ui.menu_button("Open Recent", |ui| {
            if state.recent_projects.paths.is_empty() {
                ui.add_enabled(false, egui::Button::new("Nothing yet"));
                return;
            }
            // Cloned because the loop hands `state` to `request_guarded`
            // while iterating what `state` owns.
            for root in state.recent_projects.paths.clone() {
                // The full path as a tooltip: two projects can easily be
                // called `game`, and the leaf alone would not say which.
                let label = root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string());
                let current = root == state.project.root();
                if ui
                    .add_enabled(!current, egui::Button::new(label))
                    .on_hover_text(root.display().to_string())
                    .on_disabled_hover_text("already open")
                    .clicked()
                {
                    request_guarded(state, PendingAction::OpenProject(root));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("New Scene").clicked() {
            request_guarded(state, PendingAction::NewScene);
            ui.close();
        }
        if ui.button("Revert Scene").clicked() {
            request_guarded(state, PendingAction::RevertScene);
            ui.close();
        }
        if ui.button("Save Scene").clicked() {
            match state.save_scene() {
                Ok(()) => tracing::info!(path = %state.scene_path.display(), "scene saved"),
                Err(err) => tracing::warn!(error = %err, "scene save failed"),
            }
            ui.close();
        }
        ui.add_enabled(false, egui::Button::new("Save Scene As…"));
        ui.separator();
        let configuration = state
            .selected_configuration()
            .map(|config| config.name.clone());
        let label = match &configuration {
            Some(name) if state.export_running => format!("Building {name}…"),
            Some(name) => format!("Build & Export ({name})…"),
            None => "Build & Export…".to_owned(),
        };
        let enabled = configuration.is_some() && !state.export_running && !state.play.is_playing();
        if ui
            .add_enabled(enabled, egui::Button::new(label))
            .on_hover_text("Compile the selected configuration and stage a shippable folder")
            .clicked()
        {
            if let Some(dir) = pick_project_directory("Export into") {
                state.export_request = Some(dir);
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Exit").clicked() {
            request_guarded(state, PendingAction::Quit);
            ui.close();
        }
    });
}

/// Asks the OS for a project directory.
///
/// A folder picker rather than a file picker: a project *is* a
/// directory, and asking someone to select `project.ron` inside it would
/// be asking them to know an implementation detail.
fn pick_project_directory(title: &str) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

/// Runs `action` now if nothing is unsaved; otherwise stashes it in
/// `state.pending_action` so [`unsaved_changes_modal`] can prompt.
fn request_guarded(state: &mut EditorState, action: PendingAction) {
    if state.dirty.any() {
        state.pending_action = Some(action);
    } else {
        apply_pending(state, action);
    }
}

/// Carries out a guarded action once it's cleared to proceed (nothing
/// unsaved, or the user chose Save / Discard).
fn apply_pending(state: &mut EditorState, action: PendingAction) {
    match action {
        PendingAction::Quit => state.exit_requested = true,
        PendingAction::NewScene => {
            state.new_scene();
            tracing::info!("new scene");
        }
        PendingAction::RevertScene => match state.load_scene() {
            Ok(()) => tracing::info!(path = %state.scene_path.display(), "scene reverted"),
            Err(err) => tracing::warn!(error = %err, "scene revert failed"),
        },
        // The swap itself is the shell binary's job — it owns the GPU
        // resources and the language server that also have to be torn
        // down. Recording the request here keeps that ordering explicit
        // rather than half-swapping the project from inside a menu.
        PendingAction::OpenProject(path) => {
            state.project_request = Some(ProjectRequest::open(path))
        }
        PendingAction::NewProject(path) => {
            state.project_request = Some(ProjectRequest::create(path))
        }
    }
}

/// The "unsaved changes" modal, shown whenever `state.pending_action` is
/// set. Save writes the scene then proceeds; Discard proceeds; Cancel
/// (button, backdrop, or Esc) clears the pending action.
fn unsaved_changes_modal(ui: &mut egui::Ui, state: &mut EditorState) {
    let Some(action) = state.pending_action.clone() else {
        return;
    };
    let summary = state
        .dirty
        .summary()
        .unwrap_or_else(|| "the scene".to_string());

    #[derive(Clone, Copy)]
    enum Choice {
        Proceed,
        Cancel,
    }
    let mut choice = None;

    let response = egui::Modal::new(egui::Id::new("studio_unsaved_modal")).show(ui.ctx(), |ui| {
        ui.set_width(360.0);
        ui.heading("Unsaved changes");
        ui.add_space(4.0);
        ui.label(format!(
            "Save changes to {summary} before {}?",
            action.verb()
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                match state.save_scene() {
                    Ok(()) => tracing::info!(
                        path = %state.scene_path.display(),
                        "scene saved before {}", action.verb()
                    ),
                    Err(err) => tracing::warn!(error = %err, "save-before-action failed"),
                }
                choice = Some(Choice::Proceed);
            }
            if ui.button("Discard").clicked() {
                choice = Some(Choice::Proceed);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(Choice::Cancel);
            }
        });
    });

    if response.should_close() {
        choice.get_or_insert(Choice::Cancel);
    }

    match choice {
        Some(Choice::Proceed) => {
            state.pending_action = None;
            apply_pending(state, action);
        }
        Some(Choice::Cancel) => state.pending_action = None,
        None => {}
    }
}

/// Edit menu: undo / redo, mirroring the Ctrl/Cmd+Z keybindings.
fn menu_edit(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("Edit", |ui| {
        if ui
            .add_enabled(state.history.can_undo(), egui::Button::new("Undo"))
            .clicked()
        {
            state
                .history
                .undo(&mut state.world, &mut state.selected_entity);
            state.secondary_selection.clear();
            state.dragging_axis = None;
            ui.close();
        }
        if ui
            .add_enabled(state.history.can_redo(), egui::Button::new("Redo"))
            .clicked()
        {
            state
                .history
                .redo(&mut state.world, &mut state.selected_entity);
            state.secondary_selection.clear();
            state.dragging_axis = None;
            ui.close();
        }
    });
}

/// Assets menu: import files into the project, or rescan it.
///
/// Both only raise a flag. The dialog blocks and the copy needs the
/// project directory and importer, none of which a menu closure holding
/// `&mut egui::Ui` can reach — the host acts on the flag next frame, the
/// same arrangement `create_prefab_request` uses.
fn menu_assets(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("Assets", |ui| {
        if ui.button("Import…").clicked() {
            state.import_request = true;
            ui.close();
        }
        if ui.button("Refresh").clicked() {
            state.rescan_request = true;
            ui.close();
        }
    });
}

/// GameObject menu: create an empty entity, or delete the selection —
/// both live, both mark the scene dirty.
fn menu_gameobject(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("GameObject", |ui| {
        if ui.button("Create Empty").clicked() {
            let entity = hierarchy::spawn_entity(&mut state.world, "Entity", Vec3::ZERO);
            state.select_only(entity);
            state.mark_dirty();
            ui.close();
        }
        let has_selection = state.selection_len() > 0;
        if ui
            .add_enabled(has_selection, egui::Button::new("Delete Selected"))
            .clicked()
        {
            state.delete_selection();
            state.mark_dirty();
            ui.close();
        }
    });
}

/// Component menu: add-component, pending the component registry.
fn menu_component(ui: &mut egui::Ui) {
    ui.menu_button("Component", |ui| {
        ui.add_enabled(false, egui::Button::new("Add Component…"));
    });
}

/// Tools menu: placeholder until studio tooling lands.
/// View menu: the Scene-view debug overlays, one checkbox each.
///
/// Each is independent, and all off means the frame's overlay pass adds
/// no vertices at all — the draw call is skipped, not drawn empty.
fn menu_view(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("View", |ui| {
        ui.label("Overlays");
        ui.checkbox(&mut state.overlays.colliders, "Colliders");
        ui.checkbox(&mut state.overlays.lights, "Lights");
        ui.checkbox(&mut state.overlays.cameras, "Cameras");
        ui.checkbox(&mut state.overlays.grid, "Ground grid");
        ui.checkbox(&mut state.overlays.audio, "Audio emitters");
        ui.checkbox(&mut state.overlays.nav, "Nav grid");
        ui.separator();
        if ui.button("All off").clicked() {
            state.overlays = crate::OverlayToggles::default();
            ui.close();
        }
    });
}

fn menu_tools(ui: &mut egui::Ui) {
    ui.menu_button("Tools", |ui| {
        ui.add_enabled(false, egui::Button::new("No tools yet"));
    });
}

/// Window menu: per-panel visibility toggles and the workspace presets.
fn menu_window(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("Window", |ui| {
        ui.label("Panels");
        ui.checkbox(&mut state.panels.hierarchy, "Hierarchy");
        ui.checkbox(&mut state.panels.inspector, "Inspector / Lighting");
        ui.checkbox(&mut state.panels.asset_browser, "Asset Browser");
        ui.checkbox(&mut state.asset_browser.preview_open, "Asset Preview");
        ui.checkbox(&mut state.panels.code, "Code");
        ui.checkbox(&mut state.panels.ai, "AI Assistant");
        ui.checkbox(&mut state.panels.console, "Console / Problems / Output");
        ui.checkbox(&mut state.panels.profiler, "Profiler");
        if ui.button("Reset Layouts to Presets").clicked() {
            state.reset_workspace_layouts();
            ui.close();
        }
        ui.separator();
        ui.label("Workspace");
        for workspace in Workspace::ALL {
            if ui
                .selectable_label(state.workspace == workspace, workspace.label())
                .clicked()
            {
                state.set_workspace(workspace);
                ui.close();
            }
        }
    });
}

/// Help menu: about box, pending.
fn menu_help(ui: &mut egui::Ui) {
    ui.menu_button("Help", |ui| {
        ui.add_enabled(false, egui::Button::new("About RustyEngine Studio"));
    });
}

// --- Toolbar / status bar ------------------------------------------------

/// The toolbar row: play/pause/stop, workspace switcher, gizmo mode,
/// transform space, and (right-aligned) the build configuration.
fn toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        let playing = state.play.is_playing();
        if ui
            .button(if playing { "⏹ Stop" } else { "▶ Play" })
            .clicked()
        {
            if playing {
                state.play.stop(&mut state.world);
                state.selected_entity = None;
            } else {
                state.play.start(&mut state.world);
            }
        }
        if playing {
            ui.add_enabled(false, egui::Button::new("⏸ Pause"))
                .on_disabled_hover_text(
                    "Play runs the project in a separate process; pause needs a runtime protocol",
                );
        } else {
            ui.add_enabled(false, egui::Button::new("⏸ Pause"));
        }
        if playing {
            let (tint, text) = match state.play.mode() {
                crate::play::EditorMode::Building => {
                    (egui::Color32::from_rgb(150, 170, 210), "BUILDING")
                }
                crate::play::EditorMode::Play => (egui::Color32::from_rgb(235, 180, 90), "PLAYING"),
                crate::play::EditorMode::Edit => (egui::Color32::GRAY, "EDITING"),
            };
            ui.colored_label(tint, text);
        }

        ui.separator();
        for workspace in Workspace::ALL {
            if ui
                .selectable_label(state.workspace == workspace, workspace.label())
                .clicked()
            {
                state.set_workspace(workspace);
            }
        }

        ui.separator();
        for dimension in crate::chrome::EditorDimension::ALL {
            ui.selectable_value(&mut state.dimension, dimension, dimension.label());
        }

        ui.separator();
        ui.label("Gizmo:");
        for mode in GizmoMode::ALL {
            ui.selectable_value(&mut state.gizmo_mode, mode, mode.label());
        }

        ui.separator();
        egui::ComboBox::from_id_salt("studio_transform_space")
            .selected_text(state.transform_space.label())
            .show_ui(ui, |ui| {
                for space in TransformSpace::ALL {
                    ui.selectable_value(&mut state.transform_space, space, space.label());
                }
            });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // The project's own configurations, by name — not a fixed
            // Debug/Release pair. A project says how it is built.
            let selected = state
                .selected_configuration()
                .map_or_else(|| "None".to_owned(), |config| config.name.clone());
            let names: Vec<String> = state
                .project
                .configurations()
                .iter()
                .map(|config| config.name.clone())
                .collect();
            egui::ComboBox::from_id_salt("studio_build_config")
                .selected_text(format!("Build: {selected}"))
                .show_ui(ui, |ui| {
                    for (index, name) in names.iter().enumerate() {
                        ui.selectable_value(&mut state.configuration_index, index, name);
                    }
                });
        });
    });
}

/// The bottom status strip: cursor position (placeholder until the code
/// editor exists), indentation, encoding, language, and the saved/dirty
/// indicator.
fn status_bar(ui: &mut egui::Ui, state: &EditorState) {
    ui.horizontal_centered(|ui| {
        ui.label(format!(
            "Ln {}, Col {}",
            state.cursor_line, state.cursor_column
        ));
        ui.separator();
        ui.label("Spaces: 4");
        ui.separator();
        ui.label("UTF-8");
        ui.separator();
        ui.label("Rust");
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match state.dirty.summary() {
                Some(summary) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(235, 180, 90),
                        format!("● Unsaved: {summary}"),
                    );
                }
                None => {
                    ui.weak("Saved");
                }
            },
        );
    });
}

// --- Multi-tab panels + stubs ------------------------------------------

/// The bottom Console / Problems / Output panel: a tab row plus the
/// active tab's content. The Problems tab label carries a count badge
/// when it has entries.
fn bottom_tabs_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        for tab in BottomTab::ALL {
            let label = match tab {
                BottomTab::Problems if !state.diagnostics.is_empty() => {
                    format!("Problems ({})", state.diagnostics.len())
                }
                _ => tab.label().to_string(),
            };
            ui.selectable_value(&mut state.bottom_tab, tab, label);
        }
    });
    ui.separator();
    // Each tab's `show` ends with a space-filling `ScrollArea`, so the
    // panel body reaches its bottom edge — no trailing `allocate_space`
    // here (stacking one after the ScrollArea double-claims the height
    // and makes the panel grow without bound).
    match state.bottom_tab {
        BottomTab::Console => console::show(ui, &state.console, &mut state.console_filter),
        BottomTab::Problems => {
            diagnostics::show(ui, &state.diagnostics, &mut state.problem_selected);
            state.resolve_problem_jump();
        }
        BottomTab::Output => output::show(ui, &state.output),
        BottomTab::Terminal => terminal_tab(ui, state),
        BottomTab::Search => crate::search::show(ui, &mut state.search),
    }
}

/// The Terminal tab. With the `terminal` feature it spawns a shell in
/// the project root on first view and renders it; without the feature it
/// shows a notice.
fn terminal_tab(ui: &mut egui::Ui, state: &mut EditorState) {
    #[cfg(feature = "terminal")]
    {
        if state.terminal.is_none() {
            let root = state.project.root().to_path_buf();
            match crate::terminal::Terminal::spawn(&root) {
                Ok(terminal) => state.terminal = Some(terminal),
                Err(err) => {
                    tracing::warn!(error = %err, "could not start the integrated terminal");
                }
            }
        }
        match &mut state.terminal {
            Some(terminal) => crate::terminal::show(ui, terminal),
            None => {
                ui.weak("Could not start a shell — see the Console tab for details.");
            }
        }
    }
    #[cfg(not(feature = "terminal"))]
    {
        let _ = &state;
        ui.weak("The terminal is not available in this build.");
    }
}

/// The right-side Inspector / Lighting panel: a tab row plus the active
/// tab's content. Lighting is a stub.
fn inspector_tabs_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        for tab in InspectorTab::ALL {
            ui.selectable_value(&mut state.inspector_tab, tab, tab.label());
        }
    });
    ui.separator();
    match state.inspector_tab {
        InspectorTab::Inspector => {
            inspector::show(
                ui,
                &mut state.world,
                state.selected_entity,
                &state.secondary_selection,
                &mut state.batch_edit,
                &mut state.open_script_request,
            );
        }
        InspectorTab::Lighting => {
            ui.weak("Scene lighting and environment controls appear here in a later iteration.");
        }
    }
    ui.allocate_space(ui.available_size());
}

/// Height reserved at the top of the Code panel for [`code_panel_body`]'s
/// egui header. The Monaco webview is floated below this band so the
/// panel's resize handle and the header's close button stay clickable.
#[cfg_attr(feature = "dock-shell", allow(dead_code))]
const CODE_HEADER_HEIGHT: f32 = 28.0;

/// Draws the Code panel's egui header — a faux file tab and a close
/// button — then claims the rest of the panel so it holds its
/// `default_size` (an [`egui::Panel`] otherwise shrinks to its content,
/// down to `min_size`, and persists that). Everything below the header
/// is covered by the floating Monaco webview.
///
/// `close` is set to `true` when the user clicks the ✕; the caller turns
/// that into `panels.code = false`.
fn code_panel_body(ui: &mut egui::Ui, close: &mut bool) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label("Code");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("✕")
                .on_hover_text("Hide the Code panel")
                .clicked()
            {
                *close = true;
            }
        });
    });
    ui.separator();
    ui.allocate_space(ui.available_size());
}

/// Bounded chat surface shared by the stable and docked shells. Provider
/// transport is intentionally injected by a later host layer; composing and
/// rendering a transcript never performs network I/O.
fn ai_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.heading("AI Assistant");
    ui.horizontal(|ui| {
        ui.label("Mode:");
        for mode in editor_ai::AiMode::ALL {
            ui.selectable_value(&mut state.ai_mode, mode, mode.label());
        }
    });
    ui.separator();
    for message in state.ai_chat.messages() {
        let label = match message.role {
            editor_ai::ChatRole::User => "You",
            editor_ai::ChatRole::Assistant => "Assistant",
            editor_ai::ChatRole::System => "System",
        };
        ui.label(format!("{label}: {}", message.content));
    }
    if !state.ai_chat.assistant_draft().is_empty() {
        ui.label(format!("Assistant: {}", state.ai_chat.assistant_draft()));
    }
    ui.add(
        egui::TextEdit::multiline(&mut state.ai_input)
            .hint_text("Ask about this project…")
            .desired_rows(3),
    );
    if ui.button("Queue prompt").clicked() && !state.ai_input.trim().is_empty() {
        let prompt = std::mem::take(&mut state.ai_input);
        state.ai_chat.submit_user(prompt.clone());
        state.action_log.plan(format!("chat: {prompt}"));
    }
    ui.separator();
    ui.strong("Plan / Action Log");
    for entry in state.action_log.entries().rev().take(8) {
        ui.weak(format!(
            "#{} · {:?} · {}",
            entry.id, entry.status, entry.action
        ));
    }
    ui.weak("No provider is configured; queued prompts wait for a provider.");
}

/// The Scene panel's tab strip (Scene / Game / Shaded). Display-only
/// placeholders — only the editor Scene view is implemented, so the
/// other tabs are disabled and there is nothing to switch to yet.
fn scene_view_tabs(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let _ = ui.selectable_label(true, "Scene");
        ui.add_enabled(false, egui::Button::new("Game").frame(false));
        ui.add_enabled(false, egui::Button::new("Shaded").frame(false));
    });
}

#[cfg(feature = "dock-shell")]
struct StudioDockViewer<'a> {
    state: &'a mut EditorState,
    viewport: &'a Viewport,
}

#[cfg(feature = "dock-shell")]
impl egui_dock::TabViewer for StudioDockViewer<'_> {
    type Tab = dock::Tab;

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            dock::Tab::Hierarchy => "Hierarchy",
            dock::Tab::Scene => "Scene",
            dock::Tab::Assets => "Asset Browser",
            dock::Tab::Code => "Code",
            dock::Tab::Console => "Console",
            dock::Tab::Inspector => "Inspector",
            dock::Tab::Ai => "AI Assistant",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            dock::Tab::Hierarchy => hierarchy::show(
                ui,
                &mut self.state.world,
                &mut self.state.selected_entity,
                &mut self.state.secondary_selection,
                &mut self.state.hierarchy,
            ),
            dock::Tab::Scene => {
                scene_view_tabs(ui);
                ui.separator();
                let (width, height) = self.viewport.size();
                let response = ui
                    .add(
                        egui::Image::new((
                            self.viewport.texture_id(),
                            egui::vec2(width as f32, height as f32),
                        ))
                        .sense(egui::Sense::click_and_drag()),
                    )
                    .on_hover_cursor(egui::CursorIcon::Default);
                handle_gizmo_interaction(self.viewport, self.state, &response);
                if let Some(path) = response.dnd_release_payload::<std::path::PathBuf>() {
                    let spawned = if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("prefab"))
                    {
                        self.state.spawn_prefab(&path).ok()
                    } else {
                        Some(self.state.spawn_asset_entity(&path))
                    };
                    if let Some(entity) = spawned {
                        self.state.select_only(entity);
                        self.state.mark_dirty();
                    }
                }
                ui.ctx().set_cursor_icon(scene_cursor_icon(
                    response.dragged(),
                    self.state.dragging_axis.is_some(),
                ));
            }
            dock::Tab::Assets => assets::show(
                ui,
                &self.state.assets,
                self.state.importer.stats(),
                &mut self.state.asset_browser,
                &mut self.state.selected_asset,
                &mut self.state.file_open_request,
            ),
            dock::Tab::Code => {
                let mut close = false;
                code_panel_body(ui, &mut close);
                let rect = ui.min_rect();
                self.state.code_panel_rect = Some([
                    rect.min.x,
                    rect.min.y + CODE_HEADER_HEIGHT,
                    rect.width(),
                    (rect.height() - CODE_HEADER_HEIGHT).max(1.0),
                ]);
                self.state.code_panel_visible = !close && self.state.pending_action.is_none();
            }
            dock::Tab::Console => bottom_tabs_panel(ui, self.state),
            dock::Tab::Inspector => inspector_tabs_panel(ui, self.state),
            dock::Tab::Ai => ai_panel(ui, self.state),
        }
    }

    fn is_closeable(&self, _tab: &Self::Tab) -> bool {
        false
    }
}

/* impl TabViewer for StudioDockViewer<'_> {
    type Tab = StudioTab;

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        match tab {
            StudioTab::Hierarchy => "Hierarchy",
            StudioTab::Scene => "Scene",
            StudioTab::AssetBrowser => "Asset Browser",
            StudioTab::Code => "Code",
            StudioTab::Console => "Console",
            StudioTab::Inspector => "Inspector",
            StudioTab::Ai => "AI Assistant",
            StudioTab::Profiler => "Profiler",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            StudioTab::Hierarchy => hierarchy::show(
                ui,
                &mut self.state.world,
                &mut self.state.selected_entity,
                &mut self.state.secondary_selection,
                &mut self.state.hierarchy,
            ),
            StudioTab::Scene => {
                scene_view_tabs(ui);
                ui.separator();
                let (width, height) = self.viewport.size();
                let response = ui.add(
                    egui::Image::new((
                        self.viewport.texture_id(),
                        egui::vec2(width as f32, height as f32),
                    ))
                    .sense(egui::Sense::click_and_drag()),
                );
                handle_gizmo_interaction(self.viewport, self.state, &response);
                if let Some(path) = response.dnd_release_payload::<std::path::PathBuf>() {
                    let spawned = if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("prefab"))
                    {
                        self.state.spawn_prefab(&path).ok()
                    } else {
                        Some(self.state.spawn_asset_entity(&path))
                    };
                    if let Some(entity) = spawned {
                        self.state.select_only(entity);
                        self.state.mark_dirty();
                    }
                }
            }
            StudioTab::AssetBrowser => assets::show(
                ui,
                &self.state.assets,
                self.state.importer.stats(),
                &mut self.state.asset_browser,
                &mut self.state.selected_asset,
                &mut self.state.file_open_request,
            ),
            StudioTab::Code => {
                let mut close = false;
                code_panel_body(ui, &mut close);
                let rect = ui.min_rect();
                self.state.code_panel_rect = Some([
                    rect.min.x,
                    rect.min.y + CODE_HEADER_HEIGHT,
                    rect.width(),
                    (rect.height() - CODE_HEADER_HEIGHT).max(1.0),
                ]);
                self.state.code_panel_visible = !close && self.state.pending_action.is_none();
            }
            StudioTab::Console => bottom_tabs_panel(ui, self.state),
            StudioTab::Inspector => inspector_tabs_panel(ui, self.state),
            StudioTab::Ai => {
                ui.heading("AI Assistant");
                ui.weak("No provider configured.");
            }
            StudioTab::Profiler => crate::profiler::show(ui, &self.state.profiler),
        }
    }

    fn is_closeable(&self, _tab: &Self::Tab) -> bool {
        false
    }
} */

#[cfg(test)]
mod ui_tests {
    use super::scene_cursor_icon;

    #[test]
    fn scene_is_neutral_until_a_gizmo_drag_is_active() {
        assert_eq!(scene_cursor_icon(false, false), egui::CursorIcon::Default);
        assert_eq!(
            scene_cursor_icon(true, false),
            egui::CursorIcon::Default,
            "click-dragging the scene without an axis must not show a grab cursor"
        );
        assert_eq!(scene_cursor_icon(true, true), egui::CursorIcon::Grabbing);
    }
}
