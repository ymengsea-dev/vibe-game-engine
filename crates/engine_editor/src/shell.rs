//! The editor's egui integration shell: owns the egui context, the
//! `egui-winit` event bridge, and the `egui-wgpu` renderer that draws
//! egui's output.

use egui::{Context as EguiContext, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use egui_winit::State as EguiWinitState;
use engine_platform::{Window, WindowEvent};
use glam::Vec2;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::assets;
use crate::chrome::{BottomTab, TransformSpace, Workspace};
use crate::console;
use crate::diagnostics;
use crate::dirty::PendingAction;
#[cfg(feature = "dock-shell")]
use crate::dock;
use crate::gizmo::{self, Axis, GizmoMode};
use crate::hierarchy;
use crate::inspector;
use crate::menu::{MenuCommand, MenuEffect, Overlay, Panel};
use crate::output;
use crate::project_swap::{ProjectRequest, project_creation_target};
use crate::state::EditorState;
use crate::viewport::Viewport;

const STUDIO_HEADER_HEIGHT: f32 = 36.0;
const STUDIO_STATUS_HEIGHT: f32 = 24.0;
const PANEL_HEADER_HEIGHT: f32 = 30.0;

fn studio_panel_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(crate::theme::PANEL_FILL)
        .stroke(egui::Stroke::new(1.0, crate::theme::BORDER))
        .corner_radius(egui::CornerRadius::same(9))
        .outer_margin(egui::Margin::same(3))
}

fn shell_clear_color(surface_format: wgpu::TextureFormat) -> wgpu::Color {
    let shell = crate::theme::SHELL_FILL;
    if surface_format.is_srgb() {
        let [r, g, b, _] = egui::Rgba::from(shell).to_array();
        wgpu::Color {
            r: r as f64,
            g: g as f64,
            b: b as f64,
            a: 1.0,
        }
    } else {
        wgpu::Color {
            r: f64::from(shell.r()) / 255.0,
            g: f64::from(shell.g()) / 255.0,
            b: f64::from(shell.b()) / 255.0,
            a: 1.0,
        }
    }
}

fn without_resize_line<R>(
    ui: &mut egui::Ui,
    show: impl FnOnce(&mut egui::Ui, Arc<egui::Style>) -> R,
) -> R {
    let content_style = ui.style().clone();
    let widgets = &mut ui.style_mut().visuals.widgets;
    widgets.hovered.fg_stroke = egui::Stroke::NONE;
    widgets.active.fg_stroke = egui::Stroke::NONE;
    let result = show(ui, content_style.clone());
    ui.set_style(content_style);
    result
}

trait StudioPanelExt {
    fn show_studio<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R>;

    fn show_studio_collapsible<R>(
        self,
        ui: &mut egui::Ui,
        is_expanded: &mut bool,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<R>>;
}

impl StudioPanelExt for egui::Panel {
    fn show_studio<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        without_resize_line(ui, |ui, content_style| {
            self.show(ui, |ui| {
                ui.set_style(content_style);
                add_contents(ui)
            })
        })
    }

    fn show_studio_collapsible<R>(
        self,
        ui: &mut egui::Ui,
        is_expanded: &mut bool,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<R>> {
        without_resize_line(ui, |ui, content_style| {
            self.show_collapsible(ui, is_expanded, |ui| {
                ui.set_style(content_style);
                add_contents(ui)
            })
        })
    }
}

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
    clear_color: wgpu::Color,
    repaint_deadline: Arc<Mutex<Option<Instant>>>,
    new_project: Option<NewProjectDialog>,
    #[cfg(feature = "dock-shell")]
    dock: dock::Host,
}

#[derive(Default)]
struct NewProjectDialog {
    name: String,
    error: Option<String>,
}

impl EditorShell {
    /// Invokes the same command path used by the in-window menu.
    pub fn invoke_menu_command(&mut self, state: &mut EditorState, command: MenuCommand) {
        dispatch_menu_command(state, &mut self.new_project, command);
    }
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
        wake: engine_platform::WakeHandle,
    ) -> Self {
        let context = EguiContext::default();
        let repaint_deadline = Arc::new(Mutex::new(None));
        let deadline = Arc::clone(&repaint_deadline);
        context.set_request_repaint_callback(move |info| {
            if let Ok(mut current) = deadline.lock() {
                let requested = Instant::now() + info.delay;
                if current.is_none_or(|existing| requested < existing) {
                    *current = Some(requested);
                }
            }
            let _ = wake.wake();
        });
        crate::theme::install(&context);
        let winit_state =
            EguiWinitState::new(context.clone(), ViewportId::ROOT, window, None, None, None);
        let renderer = EguiRenderer::new(device, surface_format, RendererOptions::default());
        Self {
            context,
            winit_state,
            renderer,
            clear_color: shell_clear_color(surface_format),
            repaint_deadline,
            new_project: None,
            #[cfg(feature = "dock-shell")]
            dock: dock::Host::new(),
        }
    }

    /// Returns the next repaint requested by egui.
    pub fn repaint_deadline(&self) -> Option<Instant> {
        self.repaint_deadline
            .lock()
            .ok()
            .and_then(|deadline| *deadline)
    }

    /// Clears the request consumed by the frame about to be run.
    pub fn clear_repaint_deadline(&self) {
        if let Ok(mut deadline) = self.repaint_deadline.lock() {
            *deadline = None;
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

    /// Rebinds a resized viewport to its existing egui texture id.
    pub fn update_texture(
        &mut self,
        device: &wgpu::Device,
        id: TextureId,
        view: &wgpu::TextureView,
    ) {
        self.renderer.update_egui_texture_from_wgpu_texture(
            device,
            view,
            wgpu::FilterMode::Linear,
            id,
        );
    }

    /// Runs one frame of UI, returning the output ready for
    /// [`EditorShell::render`].
    ///
    /// Draws the editor's Scene and Code layouts, with a shared top bar
    /// and status bar. Scene tools sit on the viewport; project actions
    /// remain in the menus.
    /// The central Scene panel shows `viewport`'s rendered content (call
    /// [`Viewport::render`] beforehand so the image reflects this frame,
    /// not a stale one) with the translate/rotate/scale gizmo drawn over
    /// the selection.
    ///
    /// Monaco remains a native webview over the Code panel body.
    pub fn run_frame(
        &mut self,
        window: &Window,
        viewport: &Viewport,
        state: &mut EditorState,
        show_window_menus: bool,
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
            egui::Panel::top("studio_menu_bar")
                .resizable(false)
                .exact_size(STUDIO_HEADER_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(crate::theme::BAR_FILL)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ui, |ui| {
                    studio_header_contents(ui, state, &mut self.new_project, show_window_menus);
                });
            egui::Panel::bottom("studio_status_bar")
                .resizable(false)
                .exact_size(STUDIO_STATUS_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(crate::theme::BAR_FILL)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ui, |ui| {
                    status_bar(ui, state);
                });
            #[cfg(feature = "dock-shell")]
            let _ = &mut self.dock;

            state.code_panel_rect = None;
            state.code_panel_visible = false;
            state.code_panel_resizing = false;
            state.viewport_size_request = None;
            match shell_layout(state.workspace) {
                ShellLayout::Code => show_code_workspace(ui, viewport, state),
                ShellLayout::Scene => show_scene_workspace(ui, viewport, state),
                ShellLayout::Auxiliary => {
                    // The side docks claim the full editor height. The output
                    // panel is then confined to the viewport column, matching
                    // the Scene prototype. Every panel is
                    // `show_collapsible` — drag its resize handle to the edge and
                    // it slides shut (VSCode-style); the Window-menu checkbox
                    // toggles the same `panels.*` flag. `panels.*` is copied into
                    // a local `open` first so the drag-collapse `&mut bool`
                    // doesn't alias the `state` a panel body also borrows.

                    // Left dock: Hierarchy filling the column, Asset Browser
                    // pinned to its lower half. Collapsing the dock (its own
                    // handle, or the Hierarchy checkbox) takes the Asset Browser
                    // with it.
                    let mut left_open = state.panels.hierarchy;
                    egui::Panel::left("studio_left_dock")
                        .resizable(true)
                        .default_size(240.0)
                        .min_size(170.0)
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut left_open, |ui| {
                            let mut ab_open = state.panels.asset_browser;
                            egui::Panel::bottom("studio_asset_panel")
                                .resizable(true)
                                // Order matters: `default_size` widens the size
                                // range, so the caps come after it.
                                .default_size(240.0)
                                .min_size(assets::BROWSER_MIN_HEIGHT)
                                .max_size(assets::browser_panel_max(ui.available_height()))
                                .frame(studio_panel_frame())
                                .show_separator_line(false)
                                .show_studio_collapsible(ui, &mut ab_open, |ui| {
                                    show_asset_browser(ui, state)
                                });
                            state.panels.asset_browser = ab_open;

                            show_hierarchy(ui, state);
                        });
                    state.panels.hierarchy = left_open;

                    let mut open = state.panels.inspector;
                    egui::Panel::right("studio_inspector_panel")
                        .resizable(true)
                        .default_size(275.0)
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut open, |ui| {
                            inspector_tabs_panel(ui, state)
                        });
                    state.panels.inspector = open;

                    // AI Assistant dock — a placeholder until the `editor_ai`
                    // provider/agent track lands; the panel + its toggles are
                    // wired now so the layout is stable.
                    let mut ai_open = state.panels.ai;
                    egui::Panel::right("studio_ai_panel")
                        .resizable(true)
                        .default_size(280.0)
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut ai_open, |ui| {
                            ai_panel(ui, state);
                        });
                    state.panels.ai = ai_open;

                    let mut profiler_open = state.panels.profiler;
                    egui::Panel::bottom("studio_profiler_panel")
                        .resizable(true)
                        .default_size(120.0)
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut profiler_open, |ui| {
                            panel_title(ui, ShellIcon::Profiler, "Profiler");
                            crate::profiler::show(ui, &state.profiler);
                        });
                    state.panels.profiler = profiler_open;

                    let mut open = state.panels.console;
                    egui::Panel::bottom("studio_console_panel")
                        .resizable(true)
                        .default_size(220.0)
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut open, |ui| bottom_tabs_panel(ui, state));
                    state.panels.console = open;

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
                        .frame(studio_panel_frame())
                        .show_separator_line(false)
                        .show_studio_collapsible(ui, &mut code_open, |ui| {
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
                    state.code_panel_visible = code_webview_visible(
                        state.code_panel_rect.is_some(),
                        state.pending_action.is_some(),
                        ui.ctx().any_popup_open(),
                    );

                    // Drag-resize detection: the rect moved this frame while a
                    // pointer button is held. The host hides the webview for that
                    // frame so the native view doesn't lag/overhang the edge.
                    let pointer_down = ui.input(|i| i.pointer.any_down());
                    state.code_panel_resizing = code_panel_is_resizing(
                        state.prev_code_panel_rect,
                        state.code_panel_rect,
                        pointer_down,
                    );
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
                    egui::CentralPanel::default()
                        .frame(studio_panel_frame())
                        .show(ui, |ui| show_scene_viewport(ui, viewport, state));
                }
            }
            unsaved_changes_modal(ui, state);
            new_project_modal(ui, state, &mut self.new_project);
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
            MenuCommand::Undo.apply(state);
        } else if redo_pressed {
            MenuCommand::Redo.apply(state);
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
                        load: wgpu::LoadOp::Clear(self.clear_color),
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

fn studio_header_contents(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    new_project: &mut Option<NewProjectDialog>,
    show_window_menus: bool,
) {
    ui.horizontal_centered(|ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            paint_brand_mark(ui);
            ui.label("VGE");
            ui.colored_label(crate::theme::SUBTLE, "STUDIO");
            if shell_layout(state.workspace) == ShellLayout::Scene {
                ui.add_space(10.0);
                for panel in [
                    Panel::Hierarchy,
                    Panel::AssetBrowser,
                    Panel::Console,
                    Panel::Inspector,
                ] {
                    scene_panel_toggle_button(ui, state, panel);
                }
            }
            if show_window_menus {
                ui.add_space(18.0);
                menu_file(ui, state, new_project);
                menu_edit(ui, state);
                menu_view(ui, state);
                menu_gameobject(ui, state);
                menu_assets(ui, state);
                menu_window(ui, state);
            }
            toolbar(ui, state);
        });
    });
}

fn scene_panel_toggle_button(ui: &mut egui::Ui, state: &mut EditorState, panel: Panel) {
    let (open, label) = match panel {
        Panel::Hierarchy => (state.panels.hierarchy, "Hierarchy"),
        Panel::AssetBrowser => (state.panels.asset_browser, "Project Assets"),
        Panel::Console => (state.panels.console, "Console"),
        Panel::Inspector => (state.panels.inspector, "Inspector"),
        _ => return,
    };
    let fill = if open {
        crate::theme::PANEL_RAISED
    } else {
        egui::Color32::TRANSPARENT
    };
    let response = ui
        .add(
            egui::Button::new("")
                .min_size(egui::vec2(25.0, 25.0))
                .fill(fill)
                .stroke(egui::Stroke::NONE)
                .corner_radius(5),
        )
        .on_hover_text(format!("{} {label}", if open { "Hide" } else { "Show" }));
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), open, label)
    });
    let glyph = egui::Rect::from_center_size(response.rect.center(), egui::vec2(15.0, 13.0));
    let tint = if open {
        crate::theme::ACCENT
    } else {
        crate::theme::SUBTLE
    };
    ui.painter().rect_stroke(
        glyph,
        2.0,
        egui::Stroke::new(1.0, tint),
        egui::StrokeKind::Inside,
    );
    let slot = match panel {
        Panel::Hierarchy => {
            egui::Rect::from_min_size(glyph.min + egui::vec2(2.0, 2.0), egui::vec2(3.5, 9.0))
        }
        Panel::AssetBrowser => {
            egui::Rect::from_min_size(glyph.min + egui::vec2(2.0, 7.0), egui::vec2(5.0, 4.0))
        }
        Panel::Console => {
            egui::Rect::from_min_size(glyph.min + egui::vec2(8.0, 7.0), egui::vec2(5.0, 4.0))
        }
        Panel::Inspector => {
            egui::Rect::from_min_size(glyph.min + egui::vec2(9.5, 2.0), egui::vec2(3.5, 9.0))
        }
        _ => return,
    };
    ui.painter().rect_filled(slot, 1.0, tint);
    if response.clicked() {
        let _ = MenuCommand::TogglePanel(panel).apply(state);
    }
}

const SCENE_SIDE_MIN: f32 = 160.0;
const SCENE_INSPECTOR_MIN: f32 = 200.0;
const SCENE_TOP_MIN: f32 = 140.0;
const SCENE_BOTTOM_MIN: f32 = 90.0;

fn scene_default_splits(width: f32, height: f32) -> (f32, f32, f32, f32) {
    let inspector = width * 0.20;
    let remaining = width - inspector;
    (inspector, remaining * 0.20, height * 0.40, remaining * 0.38)
}

/// The Scene prototype's four splitters belong to separate sibling panels.
fn show_scene_workspace(ui: &mut egui::Ui, viewport: &Viewport, state: &mut EditorState) {
    state.prev_code_panel_rect = None;
    let width = ui.available_width();
    let (inspector_width, hierarchy_width, bottom_height, assets_width) =
        scene_default_splits(width, ui.available_height());
    let mut inspector_open = state.panels.inspector;
    egui::Panel::right("scene_inspector_panel")
        .resizable(true)
        .default_size(inspector_width)
        .min_size(SCENE_INSPECTOR_MIN)
        .max_size((width * 0.35).max(SCENE_INSPECTOR_MIN))
        .frame(studio_panel_frame())
        .show_separator_line(false)
        .show_studio_collapsible(ui, &mut inspector_open, |ui| {
            inspector_tabs_panel(ui, state)
        });
    state.panels.inspector = inspector_open;

    show_scene_bottom_row(ui, state, bottom_height, assets_width);

    let mut hierarchy_open = state.panels.hierarchy;
    egui::Panel::left("scene_hierarchy_panel")
        .resizable(true)
        .default_size(hierarchy_width)
        .min_size(SCENE_SIDE_MIN)
        .max_size((ui.available_width() - 220.0).max(SCENE_SIDE_MIN))
        .frame(studio_panel_frame())
        .show_separator_line(false)
        .show_studio_collapsible(ui, &mut hierarchy_open, |ui| show_hierarchy(ui, state));
    state.panels.hierarchy = hierarchy_open;
    egui::CentralPanel::default()
        .frame(studio_panel_frame())
        .show(ui, |ui| show_scene_viewport(ui, viewport, state));
}

fn show_scene_bottom_row(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    bottom_height: f32,
    assets_width: f32,
) {
    let bottom_limit = (ui.available_height() - SCENE_TOP_MIN).max(SCENE_BOTTOM_MIN);
    let mut bottom_open = state.panels.asset_browser || state.panels.console;
    egui::Panel::bottom("scene_bottom_row")
        .resizable(true)
        .drag_to_open(false)
        .default_size(bottom_height)
        .min_size(SCENE_BOTTOM_MIN)
        .max_size(bottom_limit)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show_studio_collapsible(ui, &mut bottom_open, |ui| {
            let mut split_width = assets_width;
            if state.panels.asset_browser && state.panels.console {
                let row = ui.available_rect_before_wrap();
                let split_id = ui.make_persistent_id("scene_bottom_split");
                let max_assets_width = (row.width() - 220.0).max(0.0);
                let min_assets_width = SCENE_SIDE_MIN.min(max_assets_width);
                split_width = ui
                    .ctx()
                    .data_mut(|data| data.get_persisted::<f32>(split_id))
                    .unwrap_or(assets_width)
                    .clamp(min_assets_width, max_assets_width);
                let handle = egui::Rect::from_center_size(
                    egui::pos2(row.left() + split_width, row.center().y),
                    egui::vec2(10.0, row.height()),
                );
                let response = ui.interact(handle, split_id, egui::Sense::click_and_drag());
                if response.hovered() || response.dragged() {
                    ui.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if (response.dragged() || response.drag_stopped())
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let dragged_width = pointer.x - row.left();
                    if dragged_width < SCENE_SIDE_MIN {
                        state.panels.asset_browser = false;
                    } else if dragged_width > row.width() - 220.0 {
                        state.panels.console = false;
                    } else {
                        split_width = dragged_width;
                        ui.ctx()
                            .data_mut(|data| data.insert_persisted(split_id, split_width));
                    }
                }
            }
            if state.panels.asset_browser && state.panels.console {
                egui::Panel::left("scene_assets_panel")
                    .resizable(false)
                    .exact_size(split_width)
                    .frame(studio_panel_frame())
                    .show_separator_line(false)
                    .show_studio(ui, |ui| show_asset_browser(ui, state));
                egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| bottom_tabs_panel(ui, state));
            } else if state.panels.asset_browser {
                egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| show_asset_browser(ui, state));
            } else if state.panels.console {
                egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| bottom_tabs_panel(ui, state));
            }
        });
    if !bottom_open {
        state.panels.asset_browser = false;
        state.panels.console = false;
    }
}

fn show_hierarchy(ui: &mut egui::Ui, state: &mut EditorState) {
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
}

fn show_asset_browser(ui: &mut egui::Ui, state: &mut EditorState) {
    state
        .preview
        .sync_selection(if state.asset_browser.preview_open {
            state.selected_asset.as_deref()
        } else {
            None
        });
    panel_header(ui, |ui| {
        assets::folder_icon(ui);
        ui.label("Project Assets");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(11.0);
            assets::new_folder_button(ui, &mut state.asset_browser);
        });
    });
    if state.asset_browser.preview_open {
        let selected_entry = state.selected_asset.as_ref().and_then(|path| {
            state
                .assets
                .iter()
                .find(|entry| &entry.relative_path == path)
        });
        let panel_width = ui.available_width();
        egui::Panel::right("studio_asset_preview")
            .resizable(true)
            .default_size((panel_width * 0.45).clamp(80.0, 220.0))
            .min_size(80.0)
            .max_size((panel_width - 100.0).max(80.0))
            .frame(studio_panel_frame())
            .show_separator_line(false)
            .show_studio_collapsible(ui, &mut state.asset_browser.preview_open, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .min_scrolled_height(0.0)
                    .show(ui, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                crate::preview::show(
                                    ui,
                                    selected_entry,
                                    &state.importer,
                                    &mut state.preview,
                                    &state.project.assets_dir(),
                                );
                            });
                    });
            });
    }
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(7, 4))
        .show(ui, |ui| {
            assets::show_scene(
                ui,
                &state.assets,
                &mut state.asset_browser,
                &mut state.selected_asset,
                &mut state.file_open_request,
            );
            if let Some(relative) = state.asset_browser.copy_path_request.take() {
                let requested = std::path::Path::new(engine_project::ASSETS_DIR).join(relative);
                match state.project.resolve(&requested) {
                    Ok(path) => ui.ctx().copy_text(path.display().to_string()),
                    Err(err) => {
                        state.asset_browser.operation_error = Some(err.to_string());
                        ui.ctx().request_repaint();
                    }
                }
            }
        });
    state
        .preview
        .sync_selection(if state.asset_browser.preview_open {
            state.selected_asset.as_deref()
        } else {
            None
        });
}

fn show_scene_viewport(ui: &mut egui::Ui, viewport: &Viewport, state: &mut EditorState) {
    egui::Panel::bottom("scene_viewport_footer")
        .resizable(false)
        .exact_size(32.0)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 2)))
        .show_separator_line(false)
        .show(ui, |ui| scene_view_footer(ui, state));
    scene_view_toolbar(ui, state);
    let (width, height) = viewport.size();
    let image_size = ui.available_size();
    request_viewport_size(ui, viewport, state, image_size);
    let image_response = ui
        .add(
            egui::Image::new((
                viewport.texture_id(),
                egui::vec2(width as f32, height as f32),
            ))
            .fit_to_exact_size(image_size)
            .maintain_aspect_ratio(false)
            .sense(egui::Sense::click_and_drag()),
        )
        .on_hover_cursor(egui::CursorIcon::Default);
    handle_gizmo_interaction(viewport, state, &image_response);
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
    ui.ctx().set_cursor_icon(scene_cursor_icon(
        image_response.dragged(),
        state.dragging_axis.is_some(),
    ));
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
    let screen_size = Vec2::new(image_response.rect.width(), image_response.rect.height());

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
fn menu_file(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    new_project: &mut Option<NewProjectDialog>,
) {
    ui.menu_button("File", |ui| {
        if ui.button("New Project…").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::NewProject);
            ui.close();
        }
        if ui.button("Open Project…").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::OpenProject);
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
                    dispatch_menu_command(state, new_project, MenuCommand::OpenRecent(root));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("New Scene").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::NewScene);
            ui.close();
        }
        if ui.button("Revert Scene").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::RevertScene);
            ui.close();
        }
        if ui.button("Save Scene").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::SaveScene);
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
            dispatch_menu_command(state, new_project, MenuCommand::Export);
            ui.close();
        }
        ui.separator();
        if ui.button("Exit").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::Quit);
            ui.close();
        }
    });
}

fn dispatch_menu_command(
    state: &mut EditorState,
    new_project: &mut Option<NewProjectDialog>,
    command: MenuCommand,
) {
    match command.apply(state) {
        MenuEffect::None => {}
        MenuEffect::NewProjectDialog => *new_project = Some(NewProjectDialog::default()),
        MenuEffect::OpenProjectDialog => {
            if let Some(root) = pick_project_directory("Open project") {
                request_guarded(state, PendingAction::OpenProject(root));
            }
        }
        MenuEffect::ExportDialog => {
            if state.selected_configuration().is_some()
                && !state.export_running
                && !state.play.is_playing()
                && let Some(dir) = pick_project_directory("Export into")
            {
                state.export_request = Some(dir);
            }
        }
    }
}

fn new_project_modal(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    dialog: &mut Option<NewProjectDialog>,
) {
    let Some(dialog_state) = dialog.as_mut() else {
        return;
    };

    let mut close = false;
    crate::theme::modal(ui.ctx(), egui::Id::new("studio_new_project_modal"), |ui| {
        ui.set_width(360.0);
        ui.heading("Create Project");
        ui.add_space(4.0);
        ui.label("Project name");
        let name = ui.add(
            egui::TextEdit::singleline(&mut dialog_state.name)
                .hint_text("My Game")
                .desired_width(f32::INFINITY),
        );
        if name.changed() {
            dialog_state.error = None;
        }
        if let Some(error) = &dialog_state.error {
            ui.colored_label(egui::Color32::from_rgb(236, 104, 104), error);
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let valid_name = !dialog_state.name.trim().is_empty();
            if ui
                .add_enabled(
                    valid_name,
                    egui::Button::new("Choose Location…").fill(crate::theme::ACCENT_FILL),
                )
                .clicked()
                && let Some(parent) = pick_project_directory("Choose project location")
                && let Some(target) = project_creation_target(&parent, &dialog_state.name)
            {
                match target.try_exists() {
                    Ok(true) => {
                        dialog_state.error = Some(format!(
                            "{} already exists. Choose a different project name or location.",
                            target.display()
                        ));
                    }
                    Ok(false) => {
                        request_guarded(state, PendingAction::NewProject(target));
                        close = true;
                    }
                    Err(err) => {
                        dialog_state.error =
                            Some(format!("Cannot inspect the project location: {err}"));
                    }
                }
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    if close {
        *dialog = None;
    }
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
pub(crate) fn request_guarded(state: &mut EditorState, action: PendingAction) {
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

fn save_before_pending(state: &mut EditorState) -> bool {
    if state.dirty.scene() {
        match state.save_scene() {
            Ok(()) => {
                tracing::info!(path = %state.scene_path.display(), "scene saved before menu action")
            }
            Err(err) => {
                tracing::warn!(error = %err, "save-before-action failed");
                return false;
            }
        }
    }
    !state.dirty.any()
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
    let other_dirty = state.dirty.settings() || state.dirty.dirty_documents().next().is_some();

    #[derive(Clone, Copy)]
    enum Choice {
        Proceed,
        Cancel,
    }
    let mut choice = None;

    let response = crate::theme::modal(ui.ctx(), egui::Id::new("studio_unsaved_modal"), |ui| {
        ui.set_width(360.0);
        ui.heading("Unsaved changes");
        ui.add_space(4.0);
        ui.label(format!(
            "Save changes to {summary} before {}?",
            action.verb()
        ));
        if other_dirty {
            ui.weak("Save code files and project settings separately, or choose Discard.");
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("Save").fill(crate::theme::ACCENT_FILL))
                .clicked()
                && save_before_pending(state)
            {
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
            MenuCommand::Undo.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(state.history.can_redo(), egui::Button::new("Redo"))
            .clicked()
        {
            MenuCommand::Redo.apply(state);
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
            MenuCommand::Import.apply(state);
            ui.close();
        }
        if ui.button("Refresh").clicked() {
            MenuCommand::Refresh.apply(state);
            ui.close();
        }
    });
}

/// GameObject menu: create an empty entity, or delete the selection —
/// both live, both mark the scene dirty.
fn menu_gameobject(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("GameObject", |ui| {
        if ui.button("Create Empty").clicked() {
            MenuCommand::CreateEmpty.apply(state);
            ui.close();
        }
        let has_selection = state.selection_len() > 0;
        if ui
            .add_enabled(has_selection, egui::Button::new("Delete Selected"))
            .clicked()
        {
            MenuCommand::DeleteSelected.apply(state);
            ui.close();
        }
    });
}

/// View menu: the Scene-view debug overlays, one checkbox each.
///
/// Each is independent, and all off means the frame's overlay pass adds
/// no vertices at all — the draw call is skipped, not drawn empty.
fn menu_view(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("View", |ui| {
        ui.label("Overlays");
        menu_check(
            ui,
            state,
            state.overlays.colliders,
            "Colliders",
            MenuCommand::ToggleOverlay(Overlay::Colliders),
        );
        menu_check(
            ui,
            state,
            state.overlays.lights,
            "Lights",
            MenuCommand::ToggleOverlay(Overlay::Lights),
        );
        menu_check(
            ui,
            state,
            state.overlays.cameras,
            "Cameras",
            MenuCommand::ToggleOverlay(Overlay::Cameras),
        );
        menu_check(
            ui,
            state,
            state.overlays.grid,
            "Ground grid",
            MenuCommand::ToggleOverlay(Overlay::Grid),
        );
        menu_check(
            ui,
            state,
            state.overlays.audio,
            "Audio emitters",
            MenuCommand::ToggleOverlay(Overlay::Audio),
        );
        menu_check(
            ui,
            state,
            state.overlays.nav,
            "Nav grid",
            MenuCommand::ToggleOverlay(Overlay::Nav),
        );
        ui.separator();
        if ui.button("All off").clicked() {
            MenuCommand::AllOverlaysOff.apply(state);
            ui.close();
        }
    });
}

fn menu_check(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    checked: bool,
    label: &str,
    command: MenuCommand,
) {
    let mut value = checked;
    if ui.checkbox(&mut value, label).changed() {
        command.apply(state);
    }
}

/// Window menu: per-panel visibility toggles and the workspace presets.
fn menu_window(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("Window", |ui| {
        ui.label("Panels");
        if state.workspace == Workspace::Code {
            menu_check(
                ui,
                state,
                state.panels.code,
                "Code",
                MenuCommand::TogglePanel(Panel::Code),
            );
            menu_check(
                ui,
                state,
                state.panels.console,
                "Console / Problems / Output",
                MenuCommand::TogglePanel(Panel::Console),
            );
        } else {
            menu_check(
                ui,
                state,
                state.panels.hierarchy,
                "Hierarchy",
                MenuCommand::TogglePanel(Panel::Hierarchy),
            );
            menu_check(
                ui,
                state,
                state.panels.inspector,
                "Inspector",
                MenuCommand::TogglePanel(Panel::Inspector),
            );
            menu_check(
                ui,
                state,
                state.panels.asset_browser,
                "Asset Browser",
                MenuCommand::TogglePanel(Panel::AssetBrowser),
            );
            menu_check(
                ui,
                state,
                state.asset_browser.preview_open,
                "Asset Preview",
                MenuCommand::TogglePanel(Panel::AssetPreview),
            );
            if state.workspace != Workspace::World {
                menu_check(
                    ui,
                    state,
                    state.panels.code,
                    "Code",
                    MenuCommand::TogglePanel(Panel::Code),
                );
            }
            menu_check(
                ui,
                state,
                state.panels.ai,
                "AI Assistant",
                MenuCommand::TogglePanel(Panel::Ai),
            );
            menu_check(
                ui,
                state,
                state.panels.console,
                "Console / Problems / Output",
                MenuCommand::TogglePanel(Panel::Console),
            );
            menu_check(
                ui,
                state,
                state.panels.profiler,
                "Profiler",
                MenuCommand::TogglePanel(Panel::Profiler),
            );
        }
        ui.menu_button("Build Configuration", |ui| {
            let names: Vec<String> = state
                .project
                .configurations()
                .iter()
                .map(|config| config.name.clone())
                .collect();
            for (index, name) in names.iter().enumerate() {
                if ui
                    .selectable_label(state.configuration_index == index, name)
                    .clicked()
                {
                    MenuCommand::SelectConfiguration(index).apply(state);
                    ui.close();
                }
            }
        });
        if ui.button("Reset Layouts to Presets").clicked() {
            MenuCommand::ResetLayouts.apply(state);
            ui.close();
        }
        ui.separator();
        ui.label("Workspace");
        for workspace in Workspace::ALL {
            if ui
                .selectable_label(state.workspace == workspace, workspace.label())
                .clicked()
            {
                MenuCommand::SetWorkspace(workspace).apply(state);
                ui.close();
            }
        }
    });
}

// --- Toolbar / status bar ------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) enum ShellIcon {
    Run,
    Stop,
    Close,
    Layers,
    Move,
    Rotate,
    Scale,
    Code,
    Check,
    Inspector,
    Profiler,
    Viewport,
}

fn paint_brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(23.0, 23.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 5.0, egui::Color32::from_rgb(29, 45, 70));
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    let stroke = egui::Stroke::new(1.25, egui::Color32::from_rgb(131, 181, 255));
    painter.add(egui::Shape::line(
        vec![
            at(11.5, 4.8),
            at(5.4, 8.0),
            at(11.5, 11.2),
            at(17.6, 8.0),
            at(11.5, 4.8),
        ],
        stroke,
    ));
    for y in [11.4, 14.8] {
        painter.add(egui::Shape::line(
            vec![at(5.4, y), at(11.5, y + 3.3), at(17.6, y)],
            stroke,
        ));
    }
}

impl ShellIcon {
    #[cfg(test)]
    const ALL: [Self; 12] = [
        Self::Run,
        Self::Stop,
        Self::Close,
        Self::Layers,
        Self::Move,
        Self::Rotate,
        Self::Scale,
        Self::Code,
        Self::Check,
        Self::Inspector,
        Self::Profiler,
        Self::Viewport,
    ];
}

pub(crate) fn paint_shell_icon(ui: &mut egui::Ui, icon: ShellIcon) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    paint_shell_icon_at(ui, rect, icon);
}

fn paint_shell_icon_at(ui: &egui::Ui, rect: egui::Rect, icon: ShellIcon) {
    paint_shell_icon_at_color(ui, rect, icon, ui.visuals().weak_text_color());
}

fn paint_shell_icon_at_color(
    ui: &egui::Ui,
    rect: egui::Rect,
    icon: ShellIcon,
    color: egui::Color32,
) {
    let painter = ui.painter();
    let stroke = egui::Stroke::new(1.4, color);
    let inset = |x: f32, y: f32| rect.min + egui::vec2(x, y);
    match icon {
        ShellIcon::Run => {
            painter.add(egui::Shape::convex_polygon(
                vec![inset(5.0, 3.0), inset(5.0, 13.0), inset(13.0, 8.0)],
                color,
                egui::Stroke::NONE,
            ));
        }
        ShellIcon::Stop => {
            painter.rect_filled(
                egui::Rect::from_min_size(inset(4.0, 4.0), egui::vec2(8.0, 8.0)),
                1.5,
                color,
            );
        }
        ShellIcon::Close => {
            painter.line_segment([inset(4.0, 4.0), inset(12.0, 12.0)], stroke);
            painter.line_segment([inset(12.0, 4.0), inset(4.0, 12.0)], stroke);
        }
        ShellIcon::Layers => {
            painter.add(egui::Shape::line(
                vec![
                    inset(8.0, 2.0),
                    inset(2.0, 5.5),
                    inset(8.0, 9.0),
                    inset(14.0, 5.5),
                    inset(8.0, 2.0),
                ],
                stroke,
            ));
            for y in [8.5, 12.0] {
                painter.add(egui::Shape::line(
                    vec![inset(2.0, y), inset(8.0, y + 3.5), inset(14.0, y)],
                    stroke,
                ));
            }
        }
        ShellIcon::Move => {
            painter.line_segment([inset(8.0, 2.0), inset(8.0, 14.0)], stroke);
            painter.line_segment([inset(2.0, 8.0), inset(14.0, 8.0)], stroke);
            for (a, b) in [
                ((8.0, 2.0), (5.0, 5.0)),
                ((8.0, 2.0), (11.0, 5.0)),
                ((14.0, 8.0), (11.0, 5.0)),
                ((14.0, 8.0), (11.0, 11.0)),
                ((8.0, 14.0), (5.0, 11.0)),
                ((8.0, 14.0), (11.0, 11.0)),
                ((2.0, 8.0), (5.0, 5.0)),
                ((2.0, 8.0), (5.0, 11.0)),
            ] {
                painter.line_segment([inset(a.0, a.1), inset(b.0, b.1)], stroke);
            }
        }
        ShellIcon::Rotate => {
            painter.add(egui::Shape::line(
                vec![
                    inset(13.5, 7.0),
                    inset(12.5, 11.0),
                    inset(9.0, 14.0),
                    inset(5.0, 14.0),
                    inset(2.0, 11.0),
                    inset(2.0, 7.0),
                    inset(4.0, 4.0),
                    inset(8.0, 2.0),
                    inset(12.0, 3.0),
                ],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![inset(11.0, 1.0), inset(14.0, 3.0), inset(11.0, 6.0)],
                stroke,
            ));
        }
        ShellIcon::Scale => {
            painter.line_segment([inset(2.0, 14.0), inset(14.0, 2.0)], stroke);
            painter.add(egui::Shape::line(
                vec![inset(8.0, 2.0), inset(14.0, 2.0), inset(14.0, 8.0)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![inset(2.0, 8.0), inset(2.0, 14.0), inset(8.0, 14.0)],
                stroke,
            ));
        }
        ShellIcon::Code => {
            painter.add(egui::Shape::line(
                vec![inset(6.0, 4.0), inset(2.0, 8.0), inset(6.0, 12.0)],
                stroke,
            ));
            painter.add(egui::Shape::line(
                vec![inset(10.0, 4.0), inset(14.0, 8.0), inset(10.0, 12.0)],
                stroke,
            ));
            painter.line_segment([inset(9.0, 2.0), inset(7.0, 14.0)], stroke);
        }
        ShellIcon::Check => {
            painter.add(egui::Shape::line(
                vec![inset(2.0, 8.0), inset(6.0, 12.0), inset(14.0, 4.0)],
                stroke,
            ));
        }
        ShellIcon::Inspector => {
            for (x, y) in [(4.0, 6.0), (8.0, 10.0), (12.0, 5.0)] {
                painter.line_segment([inset(x, 3.0), inset(x, 13.0)], stroke);
                painter.circle_filled(inset(x, y), 1.7, color);
            }
        }
        ShellIcon::Profiler => {
            for (x, height) in [(3.0, 5.0), (7.0, 9.0), (11.0, 12.0)] {
                painter.line_segment([inset(x, 14.0), inset(x, 14.0 - height)], stroke);
            }
        }
        ShellIcon::Viewport => {
            painter.circle_stroke(inset(8.0, 8.0), 5.0, stroke);
            painter.circle_filled(inset(8.0, 8.0), 1.5, color);
        }
    }
}

fn gizmo_icon_button(ui: &mut egui::Ui, mode: GizmoMode, selected: bool) -> egui::Response {
    let icon = match mode {
        GizmoMode::Translate => ShellIcon::Move,
        GizmoMode::Rotate => ShellIcon::Rotate,
        GizmoMode::Scale => ShellIcon::Scale,
    };
    let response = ui
        .add_sized(
            [25.0, 25.0],
            egui::Button::new("")
                .fill(if selected {
                    egui::Color32::from_rgb(38, 65, 101)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::NONE)
                .corner_radius(5),
        )
        .on_hover_text(mode.label());
    paint_shell_icon_at_color(
        ui,
        egui::Rect::from_center_size(response.rect.center(), egui::vec2(16.0, 16.0)),
        icon,
        if selected {
            crate::theme::ACCENT
        } else {
            crate::theme::MUTED
        },
    );
    response
}

pub(crate) fn panel_header(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    let header = ui.horizontal(|ui| {
        ui.set_min_size(egui::vec2(width, PANEL_HEADER_HEIGHT));
        ui.add_space(11.0);
        contents(ui);
    });
    let rect = header.response.rect;
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, crate::theme::BORDER_SOFT),
    );
}

fn icon_button(ui: &mut egui::Ui, icon: ShellIcon, label: &str) -> egui::Response {
    let labeled = !label.is_empty();
    let mut button = egui::Button::new(if labeled {
        format!("    {label}")
    } else {
        "  ".to_owned()
    })
    .min_size(egui::vec2(
        if labeled { 64.0 } else { 25.0 },
        if labeled { 27.0 } else { 25.0 },
    ))
    .corner_radius(6);
    button = if labeled {
        button
            .fill(crate::theme::PANEL_RAISED)
            .stroke(egui::Stroke::new(1.0, crate::theme::BORDER))
    } else {
        button.frame(false)
    };
    let response = ui.add(button);
    let icon_rect = egui::Rect::from_center_size(
        response.rect.left_center() + egui::vec2(if labeled { 12.0 } else { 12.5 }, 0.0),
        egui::vec2(16.0, 16.0),
    );
    paint_shell_icon_at(ui, icon_rect, icon);
    response
}

fn chrome_tab(ui: &mut egui::Ui, selected: bool, label: impl Into<String>) -> egui::Response {
    let color = if selected {
        crate::theme::ACCENT
    } else {
        crate::theme::MUTED
    };
    let response = ui.add(
        egui::Button::new(egui::RichText::new(label).color(color))
            .frame(true)
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(0.0, 24.0)),
    );
    if selected {
        ui.painter().line_segment(
            [response.rect.left_bottom(), response.rect.right_bottom()],
            egui::Stroke::new(2.0, crate::theme::ACCENT),
        );
    }
    response
}

/// Play and mode controls share the prototype's single top bar.
fn toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        egui::Frame::NONE
            .fill(crate::theme::SHELL_FILL)
            .stroke(egui::Stroke::new(1.0, crate::theme::BORDER))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for workspace in Workspace::PRIMARY {
                        let selected = state.workspace == workspace;
                        let fill = if selected {
                            egui::Color32::from_rgb(44, 61, 83)
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        if ui
                            .add_sized(
                                [58.0, 22.0],
                                egui::Button::new(workspace.primary_label())
                                    .fill(fill)
                                    .stroke(egui::Stroke::NONE)
                                    .corner_radius(5),
                            )
                            .clicked()
                        {
                            state.set_workspace(workspace);
                        }
                    }
                });
            });
        ui.add_space(11.0);
        let playing = state.play.is_playing();
        if !playing {
            if icon_button(ui, ShellIcon::Run, "Run").clicked() {
                state.play.start(&mut state.world);
            }
        } else {
            if icon_button(ui, ShellIcon::Stop, "Stop").clicked() {
                state.play.stop(&mut state.world);
                state.selected_entity = None;
            }
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
    });
}

/// The bottom status strip: cursor position (placeholder until the code
/// editor exists), indentation, encoding, language, and the saved/dirty
/// indicator.
fn status_bar(ui: &mut egui::Ui, state: &EditorState) {
    ui.horizontal_centered(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot.center(), 2.5, egui::Color32::from_rgb(137, 210, 169));
        ui.label(format!(
            "Ln {}, Col {}",
            state.cursor_line, state.cursor_column
        ));
        ui.colored_label(crate::theme::SUBTLE, "·");
        ui.label("Spaces: 4");
        ui.colored_label(crate::theme::SUBTLE, "·");
        ui.label("UTF-8");
        ui.colored_label(crate::theme::SUBTLE, "·");
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
                    ui.colored_label(crate::theme::SUBTLE, "Saved");
                    paint_shell_icon(ui, ShellIcon::Check);
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
    panel_header(ui, |ui| {
        for tab in BottomTab::ALL {
            let label = match tab {
                BottomTab::Problems if !state.diagnostics.is_empty() => {
                    format!("Problems ({})", state.diagnostics.len())
                }
                _ => tab.label().to_string(),
            };
            if chrome_tab(ui, state.bottom_tab == tab, label).clicked() {
                state.bottom_tab = tab;
            }
        }
    });
    // Each tab's `show` ends with a space-filling `ScrollArea`, so the
    // panel body reaches its bottom edge — no trailing `allocate_space`
    // here (stacking one after the ScrollArea double-claims the height
    // and makes the panel grow without bound).
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(18, 8))
        .show(ui, |ui| match state.bottom_tab {
            BottomTab::Console => console::show(ui, &state.console, &mut state.console_filter),
            BottomTab::Problems => {
                diagnostics::show(ui, &state.diagnostics, &mut state.problem_selected);
                state.resolve_problem_jump();
            }
            BottomTab::Output => output::show(ui, &state.output),
            BottomTab::Terminal => terminal_tab(ui, state),
        });
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

/// The right-side Inspector. The unimplemented Lighting tab stays hidden.
fn inspector_tabs_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    panel_title(ui, ShellIcon::Inspector, "Inspector");
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 13))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    inspector::show(
                        ui,
                        &mut state.world,
                        state.selected_entity,
                        &state.secondary_selection,
                        &mut state.batch_edit,
                        &mut state.open_script_request,
                    );
                });
        });
}

fn panel_title(ui: &mut egui::Ui, icon: ShellIcon, title: &str) {
    panel_header(ui, |ui| {
        paint_shell_icon(ui, icon);
        ui.label(title);
    });
}

/// Height reserved at the top of the Code panel for [`code_panel_body`]'s
/// egui header. The Monaco webview is floated below this band so the
/// panel's resize handle and the header's close button stay clickable.
#[cfg_attr(feature = "dock-shell", allow(dead_code))]
const CODE_HEADER_HEIGHT: f32 = PANEL_HEADER_HEIGHT;

/// Draws the Code panel's egui header — a faux file tab and a close
/// button — then claims the rest of the panel so it holds its
/// `default_size` (an [`egui::Panel`] otherwise shrinks to its content,
/// down to `min_size`, and persists that). Everything below the header
/// is covered by the floating Monaco webview.
///
/// `close` is set to `true` when the user clicks the close control; the caller turns
/// that into `panels.code = false`.
fn code_panel_body(ui: &mut egui::Ui, close: &mut bool) {
    panel_header(ui, |ui| {
        paint_shell_icon(ui, ShellIcon::Code);
        ui.label("Code");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, ShellIcon::Close, "")
                .on_hover_text("Hide the Code panel")
                .clicked()
            {
                *close = true;
            }
        });
    });
    ui.allocate_space(ui.available_size());
}

/// Code mode: Monaco's project explorer and editor, rendered viewport, and output.
#[cfg_attr(feature = "dock-shell", allow(dead_code))]
fn show_code_workspace(ui: &mut egui::Ui, viewport: &Viewport, state: &mut EditorState) {
    egui::Panel::right("code_viewport_panel")
        .resizable(true)
        .default_size(280.0)
        .min_size(220.0)
        .max_size((ui.available_width() - 440.0).clamp(220.0, 600.0))
        .frame(studio_panel_frame())
        .show_separator_line(false)
        .show_studio(ui, |ui| {
            panel_title(ui, ShellIcon::Viewport, "Viewport");
            let (width, height) = viewport.size();
            let image_size = ui.available_size();
            request_viewport_size(ui, viewport, state, image_size);
            ui.add(
                egui::Image::new((
                    viewport.texture_id(),
                    egui::vec2(width as f32, height as f32),
                ))
                .fit_to_exact_size(image_size)
                .maintain_aspect_ratio(false),
            );
        });

    let mut console_open = state.panels.console;
    egui::Panel::bottom("code_console_panel")
        .resizable(true)
        .default_size(220.0)
        .min_size(80.0)
        .max_size((ui.available_height() - 140.0).clamp(80.0, 550.0))
        .frame(studio_panel_frame())
        .show_separator_line(false)
        .show_studio_collapsible(ui, &mut console_open, |ui| bottom_tabs_panel(ui, state));
    state.panels.console = console_open;

    let mut close_code = false;
    let code_shown = egui::CentralPanel::default()
        .frame(studio_panel_frame())
        .show(ui, |ui| {
            if state.panels.code {
                code_panel_body(ui, &mut close_code);
            } else {
                ui.weak("The Code panel is hidden. Reopen it from Window.");
            }
        });
    state.panels.code &= !close_code;
    state.code_panel_rect = state.panels.code.then(|| {
        let rect = code_shown.response.rect;
        let top = rect.min.y + CODE_HEADER_HEIGHT;
        [rect.min.x, top, rect.width(), (rect.max.y - top).max(1.0)]
    });
    state.code_panel_visible = code_webview_visible(
        state.code_panel_rect.is_some(),
        state.pending_action.is_some(),
        ui.ctx().any_popup_open(),
    );
    let pointer_down = ui.input(|input| input.pointer.any_down());
    state.code_panel_resizing = code_panel_is_resizing(
        state.prev_code_panel_rect,
        state.code_panel_rect,
        pointer_down,
    );
    state.prev_code_panel_rect = state.code_panel_rect;

    if state.panels.code
        && ui.input(|input| {
            (input.modifiers.command || input.modifiers.ctrl) && input.key_pressed(egui::Key::S)
        })
    {
        state.code_save_requested = true;
    }
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
    ui.label("Plan / Action Log");
    for entry in state.action_log.entries().rev().take(8) {
        ui.weak(format!(
            "#{} · {:?} · {}",
            entry.id, entry.status, entry.action
        ));
    }
    ui.weak("No provider is configured; queued prompts wait for a provider.");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "dock-shell", allow(dead_code))]
enum ShellLayout {
    Scene,
    Code,
    Auxiliary,
}

#[cfg_attr(feature = "dock-shell", allow(dead_code))]
fn shell_layout(workspace: Workspace) -> ShellLayout {
    match workspace {
        Workspace::World => ShellLayout::Scene,
        Workspace::Code => ShellLayout::Code,
        Workspace::Ai | Workspace::Debug => ShellLayout::Auxiliary,
    }
}

fn code_panel_is_resizing(
    previous: Option<[f32; 4]>,
    current: Option<[f32; 4]>,
    pointer_down: bool,
) -> bool {
    pointer_down && previous.is_some() && current != previous
}

fn code_webview_visible(panel_open: bool, action_pending: bool, popup_open: bool) -> bool {
    panel_open && !action_pending && !popup_open
}

fn viewport_render_size(size: egui::Vec2, pixels_per_point: f32) -> (u32, u32) {
    let pixels = size * pixels_per_point;
    let scale = (2048.0 / pixels.x.max(pixels.y)).min(1.0);
    (
        (pixels.x * scale).round().max(1.0) as u32,
        (pixels.y * scale).round().max(1.0) as u32,
    )
}

fn request_viewport_size(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &mut EditorState,
    size: egui::Vec2,
) {
    if ui.input(|input| input.pointer.any_down()) {
        return;
    }
    let requested = viewport_render_size(size, ui.ctx().pixels_per_point());
    let current = viewport.size();
    if requested.0.abs_diff(current.0) > 4 || requested.1.abs_diff(current.1) > 4 {
        state.viewport_size_request = Some(requested);
        ui.ctx().request_repaint();
    }
}

/// The Scene viewport header exposes only controls backed by the editor.
fn scene_view_toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    panel_header(ui, |ui| {
        panel_title_label(ui, ShellIcon::Viewport, "Viewport");
        ui.weak("/ World");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for mode in GizmoMode::ALL.into_iter().rev() {
                if gizmo_icon_button(ui, mode, state.gizmo_mode == mode).clicked() {
                    state.gizmo_mode = mode;
                }
            }
        });
    });
}

fn panel_title_label(ui: &mut egui::Ui, icon: ShellIcon, title: &str) {
    paint_shell_icon(ui, icon);
    ui.label(title);
}

fn scene_view_footer(ui: &mut egui::Ui, state: &mut EditorState) {
    let rect = ui.max_rect();
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, crate::theme::BORDER_SOFT),
    );
    ui.horizontal(|ui| {
        for dimension in crate::chrome::EditorDimension::ALL {
            if chrome_tab(ui, state.dimension == dimension, dimension.label()).clicked() {
                state.dimension = dimension;
            }
        }
        egui::ComboBox::from_id_salt("studio_transform_space")
            .selected_text(state.transform_space.label())
            .show_ui(ui, |ui| {
                for space in TransformSpace::ALL {
                    ui.selectable_value(&mut state.transform_space, space, space.label());
                }
            });
        ui.menu_button("Overlays", |ui| {
            ui.checkbox(&mut state.overlays.colliders, "Colliders");
            ui.checkbox(&mut state.overlays.lights, "Lights");
            ui.checkbox(&mut state.overlays.cameras, "Cameras");
            ui.checkbox(&mut state.overlays.grid, "Ground grid");
            ui.checkbox(&mut state.overlays.audio, "Audio emitters");
            ui.checkbox(&mut state.overlays.nav, "Nav grid");
        });
        ui.menu_button("Terrain", |ui| {
            crate::terrain_tools::show_controls(ui, &mut state.terrain_tools);
        });
    });
}

#[cfg(feature = "dock-shell")]
#[allow(dead_code)]
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
                scene_view_toolbar(ui, self.state);
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
                self.state.code_panel_visible = code_webview_visible(
                    !close,
                    self.state.pending_action.is_some(),
                    ui.ctx().any_popup_open(),
                );
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
    use super::StudioPanelExt;
    use super::{
        ShellIcon, ShellLayout, code_panel_is_resizing, code_webview_visible, paint_shell_icon,
        scene_cursor_icon, shell_layout, viewport_render_size,
    };
    use crate::chrome::Workspace;

    #[test]
    fn scene_split_limits_preserve_each_panels_minimum() {
        assert_eq!(super::SCENE_SIDE_MIN, 160.0);
        assert_eq!(super::SCENE_INSPECTOR_MIN, 200.0);
        assert_eq!(super::SCENE_TOP_MIN, 140.0);
        assert_eq!(super::SCENE_BOTTOM_MIN, 90.0);
    }

    #[test]
    fn scene_defaults_scale_with_available_space() {
        let small = super::scene_default_splits(1512.0, 790.0);
        let large = super::scene_default_splits(2016.0, 1053.0);
        assert!((small.0 - 302.4).abs() < 0.001);
        assert!((small.1 - 241.92).abs() < 0.001);
        assert!((small.2 - 316.0).abs() < 0.001);
        assert!((small.3 - 459.648).abs() < 0.001);
        assert!((large.0 / small.0 - 2016.0 / 1512.0).abs() < 0.001);
        assert!((large.2 / small.2 - 1053.0 / 790.0).abs() < 0.001);
    }

    fn menu_test_state() -> (std::path::PathBuf, crate::EditorState) {
        let root = std::env::temp_dir().join(format!(
            "vge_menu_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let project = engine_project::Project::create(&root, "Menu test").expect("project");
        let state = crate::EditorState::new(
            project,
            engine_ecs::prelude::World::new(),
            Vec::new(),
            crate::ConsoleLog::new(),
        );
        (root, state)
    }

    #[test]
    fn selected_asset_preview_is_to_the_right_of_the_tree() {
        let (root, mut state) = menu_test_state();
        state.assets.push(crate::assets::AssetEntry {
            relative_path: std::path::PathBuf::from("player.png"),
            kind: crate::assets::AssetKind::Texture,
            id: None,
        });
        state.selected_asset = Some(std::path::PathBuf::from("player.png"));
        let ctx = egui::Context::default();
        let mut tree = None;
        let mut preview = None;
        for frame in 0..8 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(240.0, 260.0),
                )),
                time: Some(f64::from(frame) * 0.1),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| super::show_asset_browser(ui, &mut state));
            if frame == 7 {
                for shape in &output.shapes {
                    if let egui::epaint::Shape::Text(text) = &shape.shape {
                        let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                        match text.galley.text().trim() {
                            "assets" => tree = Some(rect),
                            "Preview" => preview = Some(rect),
                            _ => {}
                        }
                    }
                }
            }
            output.textures_delta.clear();
        }
        let tree = tree.expect("root of asset tree");
        let preview = preview.expect("selected asset preview");
        assert!(
            preview.min.x > tree.max.x,
            "tree {tree:?}, preview {preview:?}"
        );
        assert!((preview.min.y - tree.min.y).abs() < 30.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn asset_preview_stays_visible_without_selection() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(460.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| super::show_asset_browser(ui, &mut state),
        );
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        assert!(labels.iter().any(|label| label == "Preview"));
        assert!(labels.iter().any(|label| label == "No item selected"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_assets_title_uses_a_folder_outline_instead_of_placeholder_glyph() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::show_asset_browser(ui, &mut state);
        });
        let placeholder = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text() == "▱",
            _ => false,
        });
        let title = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "Project Assets" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        let title = title.expect("Project Assets title");
        let folder_outline = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Path(path) => {
                path.points.len() >= 5
                    && path
                        .points
                        .iter()
                        .all(|point| point.x < title.min.x && point.y < title.max.y)
            }
            _ => false,
        });
        output.textures_delta.clear();
        assert!(
            !placeholder,
            "Project Assets title still uses the placeholder icon"
        );
        assert!(folder_outline, "Project Assets title has no folder outline");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn new_folder_button_shares_the_project_assets_header_row() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::show_asset_browser(ui, &mut state);
        });
        let label_rect = |label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text().contains(label) => {
                    Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                }
                _ => None,
            })
        };
        let title_y = label_rect("Project Assets")
            .expect("asset title")
            .center()
            .y;
        let button = label_rect("Folder").expect("new folder button");
        let clean_label = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => text.galley.text() == "Folder",
            _ => false,
        });
        let button_y = button.center().y;
        output.textures_delta.clear();
        assert!(clean_label);
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events: vec![egui::Event::PointerButton {
                        pos: button.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| super::show_asset_browser(ui, &mut state),
            );
            output.textures_delta.clear();
        }
        let _ = std::fs::remove_dir_all(root);
        assert!(
            (title_y - button_y).abs() <= 5.0,
            "title {title_y}, button {button_y}"
        );
        assert_eq!(state.asset_browser.new_folder, Some(String::new()));
    }

    #[test]
    fn copy_path_request_uses_the_absolute_project_asset_path() {
        let (root, mut state) = menu_test_state();
        state.asset_browser.copy_path_request = Some(std::path::PathBuf::from("sprite.png"));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::show_asset_browser(ui, &mut state);
        });
        let expected = state.project.assets_dir().join("sprite.png");
        let copied = output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == &expected.display().to_string())
        });
        output.textures_delta.clear();
        let _ = std::fs::remove_dir_all(root);
        assert!(copied);
    }

    #[test]
    fn menu_quit_requires_confirmation_when_scene_dirty() {
        let (root, mut state) = menu_test_state();
        state.mark_dirty();

        crate::MenuCommand::Quit.apply(&mut state);

        assert_eq!(state.pending_action, Some(crate::PendingAction::Quit));
        assert!(!state.exit_requested);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_scene_and_project_actions_keep_the_unsaved_guard() {
        let (root, mut state) = menu_test_state();
        state.mark_dirty();

        crate::MenuCommand::NewScene.apply(&mut state);
        assert_eq!(state.pending_action, Some(crate::PendingAction::NewScene));
        state.pending_action = None;

        crate::MenuCommand::RevertScene.apply(&mut state);
        assert_eq!(
            state.pending_action,
            Some(crate::PendingAction::RevertScene)
        );
        state.pending_action = None;

        let other = root.join("other-project");
        crate::MenuCommand::OpenRecent(other.clone()).apply(&mut state);
        assert_eq!(
            state.pending_action,
            Some(crate::PendingAction::OpenProject(other))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_save_scene_leaves_dirty_code_document_unsaved() {
        let (root, mut state) = menu_test_state();
        state.mark_dirty();
        state.dirty.mark_document("src/main.rs");

        crate::MenuCommand::SaveScene.apply(&mut state);

        assert!(!state.dirty.scene());
        assert!(state.dirty.any());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_save_before_guarded_action_keeps_dirty_code_safe() {
        let (root, mut state) = menu_test_state();
        state.mark_dirty();
        state.dirty.mark_document("src/main.rs");
        state.pending_action = Some(crate::PendingAction::Quit);

        assert!(!super::save_before_pending(&mut state));
        assert_eq!(state.pending_action, Some(crate::PendingAction::Quit));
        assert!(state.dirty.any());
        assert!(!state.exit_requested);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_undo_and_redo_clear_secondary_selection() {
        let (root, mut state) = menu_test_state();
        state.history.begin_frame(&mut state.world, None);
        let entity = state
            .world
            .spawn(engine_ecs::components::Name::new("Cube"))
            .id();
        assert!(state.history.settle(&mut state.world, Some(entity)));
        state.selected_entity = Some(entity);
        state.secondary_selection.push(entity);

        crate::MenuCommand::Undo.apply(&mut state);
        assert!(state.secondary_selection.is_empty());
        assert!(state.history.can_redo());
        state.secondary_selection.push(entity);
        crate::MenuCommand::Redo.apply(&mut state);
        assert!(state.secondary_selection.is_empty());
        assert!(state.history.can_undo());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn primary_toolbar_maps_to_only_scene_and_code_layouts() {
        assert_eq!(shell_layout(Workspace::World), ShellLayout::Scene);
        assert_eq!(shell_layout(Workspace::Code), ShellLayout::Code);
        assert_eq!(shell_layout(Workspace::Ai), ShellLayout::Auxiliary);
        assert_eq!(shell_layout(Workspace::Debug), ShellLayout::Auxiliary);
    }

    #[test]
    fn code_panel_resizes_only_during_a_pointer_drag_that_changes_its_rect() {
        let previous = Some([10.0, 20.0, 300.0, 400.0]);
        assert!(!code_panel_is_resizing(None, previous, true));
        assert!(!code_panel_is_resizing(previous, previous, true));
        assert!(!code_panel_is_resizing(
            previous,
            Some([10.0, 20.0, 320.0, 400.0]),
            false
        ));
        assert!(code_panel_is_resizing(
            previous,
            Some([10.0, 20.0, 320.0, 400.0]),
            true
        ));
    }

    #[test]
    fn code_webview_releases_mouse_input_to_egui_popups() {
        assert!(code_webview_visible(true, false, false));
        assert!(!code_webview_visible(true, false, true));
        assert!(!code_webview_visible(true, true, false));
        assert!(!code_webview_visible(false, false, false));
    }

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

    #[test]
    fn viewport_render_size_follows_panel_aspect_without_exceeding_gpu_budget() {
        assert_eq!(
            viewport_render_size(egui::vec2(800.0, 400.0), 2.0),
            (1600, 800)
        );
        assert_eq!(
            viewport_render_size(egui::vec2(1600.0, 800.0), 2.0),
            (2048, 1024)
        );
        assert_eq!(viewport_render_size(egui::vec2(0.0, 0.0), 2.0), (1, 1));
    }

    #[test]
    fn shell_icons_paint_vector_geometry() {
        for icon in ShellIcon::ALL {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_shell_icon(ui, icon);
            });
            assert!(
                !output.shapes.is_empty(),
                "{icon:?} did not paint any vector geometry"
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn scene_gizmo_controls_are_vector_icons_with_tooltips() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::scene_view_toolbar(ui, &mut state);
        });
        let old_text_controls = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => {
                matches!(text.galley.text(), "Move" | "Rotate" | "Scale")
            }
            _ => false,
        });
        output.textures_delta.clear();
        assert!(!old_text_controls);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn studio_chrome_uses_compact_consistent_bands() {
        assert_eq!(super::STUDIO_HEADER_HEIGHT, 36.0);
        assert_eq!(super::STUDIO_STATUS_HEIGHT, 24.0);
        assert_eq!(super::PANEL_HEADER_HEIGHT, 30.0);
    }

    #[test]
    fn studio_tabs_have_a_consistent_control_height() {
        let ctx = egui::Context::default();
        let mut tab_height = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            tab_height = super::chrome_tab(ui, true, "Scene").rect.height();
        });
        output.textures_delta.clear();
        assert_eq!(tab_height, 24.0);
    }

    #[test]
    fn studio_tab_labels_have_horizontal_padding() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut tab_width = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            tab_width = super::chrome_tab(ui, true, "Console").rect.width();
        });
        let label_width = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Console" => {
                    Some(text.galley.size().x)
                }
                _ => None,
            })
            .expect("Console tab text");
        output.textures_delta.clear();
        assert!(
            tab_width - label_width >= 16.0,
            "tab width {tab_width}, label width {label_width}"
        );
    }

    #[test]
    fn native_menu_hides_duplicate_window_menus_but_keeps_toolbar() {
        let (root, mut state) = menu_test_state();
        for (show_window_menus, expected_file_menu) in [(false, false), (true, true)] {
            let ctx = egui::Context::default();
            let mut new_project = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                super::studio_header_contents(ui, &mut state, &mut new_project, show_window_menus);
            });
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
            assert_eq!(
                labels.iter().any(|label| label == "File"),
                expected_file_menu
            );
            assert!(labels.iter().any(|label| label == "VGE"));
            assert!(labels.iter().any(|label| label.trim() == "Run"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn studio_title_is_centered_vertically_in_the_header() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut header = egui::Rect::NOTHING;
        let mut new_project = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                header = egui::Panel::top("test_studio_header")
                    .exact_size(super::STUDIO_HEADER_HEIGHT)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(14, 0)))
                    .show(ui, |ui| {
                        super::studio_header_contents(ui, &mut state, &mut new_project, false);
                    })
                    .response
                    .rect;
            },
        );
        let title = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "VGE" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        });
        output.textures_delta.clear();
        let title = title.expect("studio title");
        assert!(
            (title.center().y - header.center().y).abs() <= 2.0,
            "title {title:?}, header {header:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn four_header_controls_toggle_the_scene_panels() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut new_project = None;
        {
            let mut draw = |time: f64, events: Vec<egui::Event>| {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::Panel::top("test_panel_toggles")
                            .exact_size(super::STUDIO_HEADER_HEIGHT)
                            .show(ui, |ui| {
                                super::studio_header_contents(
                                    ui,
                                    &mut state,
                                    &mut new_project,
                                    false,
                                );
                            });
                    },
                )
            };
            let mut output = draw(0.0, Vec::new());
            let studio_right = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == "STUDIO" => {
                        Some(text.pos.x + text.galley.size().x)
                    }
                    _ => None,
                })
                .expect("STUDIO label");
            let mut icons: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.fill == crate::theme::ACCENT
                            && rect.rect.width() <= 9.0
                            && rect.rect.height() <= 9.0
                            && rect.rect.min.x > studio_right
                            && rect.rect.max.x < studio_right + 160.0 =>
                    {
                        Some(rect.rect.center())
                    }
                    _ => None,
                })
                .collect();
            icons.sort_by(|a, b| a.x.total_cmp(&b.x));
            output.textures_delta.clear();
            assert_eq!(icons.len(), 4, "expected four vector panel controls");
            for (index, icon) in icons.into_iter().enumerate() {
                for pressed in [true, false] {
                    let mut output = draw(
                        index as f64 + if pressed { 0.1 } else { 0.2 },
                        vec![egui::Event::PointerButton {
                            pos: icon,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                    output.textures_delta.clear();
                }
            }
        }
        assert_eq!(
            (
                state.panels.hierarchy,
                state.panels.asset_browser,
                state.panels.console,
                state.panels.inspector,
            ),
            (false, false, false, false)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dragging_the_shared_bottom_edge_hides_whichever_bottom_panels_are_visible() {
        for (assets_open, console_open) in [(true, true), (true, false), (false, true)] {
            let (root, mut state) = menu_test_state();
            state.panels.asset_browser = assets_open;
            state.panels.console = console_open;
            let ctx = egui::Context::default();
            let start = egui::pos2(500.0, 380.0);
            let end = egui::pos2(500.0, 595.0);
            let frames = [
                Vec::new(),
                vec![egui::Event::PointerMoved(start)],
                vec![egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                vec![egui::Event::PointerMoved(end)],
                vec![egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                Vec::new(),
            ];
            for (frame, events) in frames.into_iter().enumerate() {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        time: Some(frame as f64 * 0.1),
                        events,
                        ..Default::default()
                    },
                    |ui| super::show_scene_bottom_row(ui, &mut state, 220.0, 304.0),
                );
                output.textures_delta.clear();
            }
            assert!(!state.panels.asset_browser);
            assert!(!state.panels.console);
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn dragging_the_assets_edge_left_hides_only_project_assets() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let start = egui::pos2(304.0, 450.0);
        let end = egui::pos2(2.0, 450.0);
        let frames = [
            Vec::new(),
            vec![egui::Event::PointerMoved(start)],
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![egui::Event::PointerMoved(end)],
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            Vec::new(),
        ];
        for (frame, events) in frames.into_iter().enumerate() {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    events,
                    ..Default::default()
                },
                |ui| super::show_scene_bottom_row(ui, &mut state, 220.0, 304.0),
            );
            output.textures_delta.clear();
        }
        assert!(!state.panels.asset_browser);
        assert!(state.panels.console);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_assets_fills_bottom_row_when_console_is_hidden() {
        let (root, mut state) = menu_test_state();
        state.panels.console = false;
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| super::show_scene_bottom_row(ui, &mut state, 220.0, 304.0),
        );
        let assets_card = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(rect)
                if rect.fill == crate::theme::PANEL_FILL && rect.rect.height() > 180.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            assets_card.is_some_and(|rect| rect.width() > 760.0),
            "assets card did not fill bottom row: {assets_card:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dragging_console_edge_right_hides_console_without_hiding_assets() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let start = egui::pos2(304.0, 450.0);
        let end = egui::pos2(798.0, 450.0);
        let frames = [
            Vec::new(),
            vec![egui::Event::PointerMoved(start)],
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![egui::Event::PointerMoved(end)],
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            Vec::new(),
        ];
        for (frame, events) in frames.into_iter().enumerate() {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    events,
                    ..Default::default()
                },
                |ui| super::show_scene_bottom_row(ui, &mut state, 220.0, 304.0),
            );
            output.textures_delta.clear();
        }
        assert!(!state.panels.console);
        assert!(state.panels.asset_browser);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dock_panels_use_the_approved_prototype_card_boundary() {
        let frame = super::studio_panel_frame();
        assert_eq!(frame.fill, egui::Color32::from_rgb(21, 29, 41));
        assert_eq!(frame.corner_radius, egui::CornerRadius::same(9));
        assert_eq!(frame.stroke.width, 1.0);
        assert_eq!(frame.stroke.color, egui::Color32::from_rgb(43, 59, 80));
        assert_eq!(frame.inner_margin, egui::Margin::ZERO);
        assert_eq!(frame.outer_margin, egui::Margin::same(3));
    }

    #[test]
    fn srgb_surface_clear_matches_the_dark_shell_gap() {
        let clear = super::shell_clear_color(wgpu::TextureFormat::Bgra8UnormSrgb);
        assert!((clear.r - 0.003_347).abs() < 0.000_01);
        assert!((clear.g - 0.005_605).abs() < 0.000_01);
        assert!((clear.b - 0.010_330).abs() < 0.000_01);
        assert_eq!(clear.a, 1.0);
    }

    #[test]
    fn linear_surface_clear_keeps_the_shell_rgb_values() {
        let clear = super::shell_clear_color(wgpu::TextureFormat::Bgra8Unorm);
        assert!((clear.r - 11.0 / 255.0).abs() < f64::EPSILON);
        assert!((clear.g - 17.0 / 255.0).abs() < f64::EPSILON);
        assert!((clear.b - 26.0 / 255.0).abs() < f64::EPSILON);
        assert_eq!(clear.a, 1.0);
    }

    #[test]
    fn nested_console_card_keeps_its_right_corner_inside_the_clip() {
        use tracing_subscriber::prelude::*;

        let (root, mut state) = menu_test_state();
        let subscriber = tracing_subscriber::registry()
            .with(crate::console::ConsoleLayer::new(state.console.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("{}", "very long console message ".repeat(100));
        });
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Panel::right("test_inspector")
                .exact_size(220.0)
                .frame(super::studio_panel_frame())
                .show(ui, |_| {});
            egui::Panel::bottom("test_bottom_row")
                .exact_size(220.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    egui::Panel::left("test_assets")
                        .exact_size(240.0)
                        .frame(super::studio_panel_frame())
                        .show(ui, |_| {});
                    egui::CentralPanel::default()
                        .frame(super::studio_panel_frame())
                        .show(ui, |ui| super::bottom_tabs_panel(ui, &mut state));
                });
        });
        let console = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == crate::theme::PANEL_FILL
                        && rect.stroke.color == crate::theme::BORDER
                        && rect.rect.width() > 400.0 =>
                {
                    Some((rect.rect, clipped.clip_rect))
                }
                _ => None,
            });
        output.textures_delta.clear();
        let Some((card, clip)) = console else {
            panic!("console card was not painted");
        };
        assert!(
            card.max.x <= clip.max.x - 2.0,
            "card {card:?}, clip {clip:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn inspector_empty_state_keeps_space_from_panel_edge() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut left = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            left = ui.max_rect().left();
            super::inspector_tabs_panel(ui, &mut state);
        });
        let label_x = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "No entity selected." => {
                Some(text.pos.x)
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            label_x.is_some_and(|x| x - left >= 12.0),
            "{label_x:?} from {left}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn console_content_has_compact_side_padding() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut left = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            left = ui.max_rect().left();
            super::bottom_tabs_panel(ui, &mut state);
        });
        let label_x = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "No log output yet." => {
                Some(text.pos.x)
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            label_x.is_some_and(|x| x - left >= 18.0),
            "{label_x:?} from {left}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bottom_panel_does_not_show_project_search_tab() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            super::bottom_tabs_panel(ui, &mut state);
        });
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        for tab in ["Console", "Problems", "Output", "Terminal"] {
            assert!(labels.iter().any(|label| label == tab), "missing {tab} tab");
        }
        assert!(!labels.iter().any(|label| label == "Search"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn asset_tree_keeps_space_from_panel_edge() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut left = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            left = ui.max_rect().left();
            super::show_asset_browser(ui, &mut state);
        });
        let label_x = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == "assets" => Some(text.pos.x),
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            label_x.is_some_and(|x| x - left >= 56.0),
            "{label_x:?} from {left}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hierarchy_root_keeps_space_from_panel_edge() {
        let (root, mut state) = menu_test_state();
        let ctx = egui::Context::default();
        let mut left = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            left = ui.max_rect().left();
            super::show_hierarchy(ui, &mut state);
        });
        let label_x = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text().contains("Scene root") => {
                Some(text.pos.x)
            }
            _ => None,
        });
        output.textures_delta.clear();
        assert!(
            label_x.is_some_and(|x| x - left >= 9.0),
            "{label_x:?} from {left}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hovering_or_dragging_a_panel_resize_edge_does_not_paint_a_white_line() {
        let ctx = egui::Context::default();
        let mut edge = 0.0;
        let mut initial_edge = 0.0;
        for frame in 0..4 {
            let events = match frame {
                0 => vec![],
                1 => vec![egui::Event::PointerMoved(egui::pos2(edge, 80.0))],
                2 => vec![egui::Event::PointerButton {
                    pos: egui::pos2(edge, 80.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                _ => vec![egui::Event::PointerMoved(egui::pos2(edge + 10.0, 80.0))],
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let shown = egui::Panel::left("resize_test")
                        .resizable(true)
                        .default_size(120.0)
                        .show_separator_line(false)
                        .show_studio(ui, |ui| {
                            assert!(ui.visuals().widgets.hovered.fg_stroke.width > 0.0);
                            ui.allocate_space(ui.available_size())
                        });
                    edge = shown.response.rect.right();
                },
            );
            if frame == 0 {
                initial_edge = edge;
            }
            if frame == 3 {
                assert!(
                    edge > initial_edge,
                    "resize drag did not move the panel edge"
                );
            }
            if frame == 1 || frame == 3 {
                assert_eq!(
                    output.platform_output.cursor_icon,
                    egui::CursorIcon::ResizeHorizontal
                );
                let style = ctx.style_of(egui::Theme::Dark);
                let hover_color = style.visuals.widgets.hovered.fg_stroke.color;
                let active_color = style.visuals.widgets.active.fg_stroke.color;
                assert!(output.shapes.iter().all(|shape| match &shape.shape {
                    egui::epaint::Shape::LineSegment { points, stroke } => {
                        !(stroke.width > 0.0
                            && (stroke.color == hover_color || stroke.color == active_color)
                            && (points[0].x - edge).abs() <= 2.0
                            && (points[1].x - edge).abs() <= 2.0)
                    }
                    _ => true,
                }));
            }
            output.textures_delta.clear();
        }
    }
}
