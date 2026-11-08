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
use crate::chrome::{BottomTab, TransformSpace, ViewTool, Workspace};
use crate::console;
use crate::diagnostics;
use crate::dirty::PendingAction;
#[cfg(feature = "dock-shell")]
use crate::dock;
use crate::gizmo::{self, Axis, GizmoHandle};
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

/// The frame every Studio panel sits in: a soft card on the darker window
/// surface (see `docs/dev/04_UI_STYLE.md`).
fn studio_panel_frame() -> egui::Frame {
    let palette = crate::theme::NIGHT;
    egui::Frame::NONE
        .fill(palette.panel)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(10))
        .outer_margin(egui::Margin::same(PANEL_GAP_HALF))
}

/// Half the gap between two panels: each panel card keeps this much space
/// around itself, so neighbours sit two of these apart.
const PANEL_GAP_HALF: i8 = 2;

fn shell_clear_color(surface_format: wgpu::TextureFormat) -> wgpu::Color {
    let shell = crate::theme::NIGHT.shell;
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

    /// [`Self::show_studio_collapsible`] without the resize grip, for a
    /// frameless container whose caller draws the grip over the cards
    /// inside it.
    fn show_studio_collapsible_bare<R>(
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
        let parent = ui.available_rect_before_wrap();
        let shown = without_resize_line(ui, |ui, content_style| {
            self.show_collapsible(ui, is_expanded, |ui| {
                ui.set_style(content_style);
                add_contents(ui)
            })
        });
        if let Some(shown) = &shown {
            paint_resize_grip(ui, parent, shown.response.rect);
        }
        shown
    }

    fn show_studio_collapsible_bare<R>(
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

/// The edge of a panel that resizes it, found from where the panel sits in
/// the `parent` area it was laid out in: a panel against the left edge
/// resizes from its right edge, and so on. `None` when the panel fills the
/// area, so has no free edge.
fn resize_edge(parent: egui::Rect, panel: egui::Rect) -> Option<ResizeEdge> {
    const EPS: f32 = 1.0;
    let tall = (panel.height() - parent.height()).abs() < EPS;
    let wide = (panel.width() - parent.width()).abs() < EPS;
    if tall && !wide {
        if (panel.left() - parent.left()).abs() < EPS {
            Some(ResizeEdge::Vertical(panel.right(), panel.y_range()))
        } else {
            Some(ResizeEdge::Vertical(panel.left(), panel.y_range()))
        }
    } else if wide && !tall {
        if (panel.top() - parent.top()).abs() < EPS {
            Some(ResizeEdge::Horizontal(panel.bottom(), panel.x_range()))
        } else {
            Some(ResizeEdge::Horizontal(panel.top(), panel.x_range()))
        }
    } else {
        None
    }
}

/// A panel's resize edge: a vertical one at an x, or a horizontal one at a
/// y, with the range it spans. It runs down the middle of the gap between
/// two panels.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ResizeEdge {
    Vertical(f32, egui::Rangef),
    Horizontal(f32, egui::Rangef),
}

/// Shows that a panel edge can be dragged: three small dots in the middle of
/// the gap, and while the pointer is over the edge (egui's own grab zone)
/// or dragging it, a blue line along it instead. egui already sets the
/// native resize cursor there.
fn paint_resize_grip(ui: &egui::Ui, parent: egui::Rect, panel: egui::Rect) {
    if let Some(edge) = resize_edge(parent, panel) {
        paint_grips(ui, &[edge], None);
    }
}

/// Draws the grip of one resizable edge split into `edges`, one per card
/// along it (as above the Scene workspace's bottom row, where one drag
/// resizes both Project Assets and the Console): dots in the middle of each,
/// or, while the edge is hovered or dragged, a line along each. `handle` is
/// the edge's own drag response when the caller has one; otherwise the
/// pointer is checked against egui's grab zone around each edge.
fn paint_grips(ui: &egui::Ui, edges: &[ResizeEdge], handle: Option<&egui::Response>) {
    let palette = crate::theme::NIGHT;
    let grab = ui.style().interaction.resize_grab_radius_side;
    let inset = 10.0 + f32::from(PANEL_GAP_HALF);
    let parts = |edge: &ResizeEdge| match *edge {
        ResizeEdge::Vertical(x, range) => (
            egui::Rect::from_x_y_ranges(egui::Rangef::new(x - grab, x + grab), range),
            egui::vec2(0.0, 1.0),
            egui::pos2(x, range.center()),
            range,
        ),
        ResizeEdge::Horizontal(y, range) => (
            egui::Rect::from_x_y_ranges(range, egui::Rangef::new(y - grab, y + grab)),
            egui::vec2(1.0, 0.0),
            egui::pos2(range.center(), y),
            range,
        ),
    };
    let (over, dragging) = match handle {
        Some(handle) => (handle.hovered() || handle.dragged(), handle.dragged()),
        None => {
            let pointer = ui.ctx().pointer_latest_pos();
            // Over the edge, and not under a window or menu that covers it.
            let over = pointer.is_some_and(|pointer| {
                edges.iter().any(|edge| parts(edge).0.contains(pointer))
                    && ui.ctx().layer_id_at(pointer) == Some(ui.layer_id())
            });
            (
                over,
                over && ui.ctx().input(|input| input.pointer.primary_down()),
            )
        }
    };
    let painter = ui.painter();
    for edge in edges {
        let (_, along, middle, range) = parts(edge);
        if over {
            let half = (range.span() / 2.0 - inset).max(0.0);
            let color = if dragging {
                palette.accent
            } else {
                palette.accent.gamma_multiply(0.55)
            };
            painter.line_segment(
                [middle - along * half, middle + along * half],
                egui::Stroke::new(2.0, color),
            );
        } else {
            for step in [-1.0, 0.0, 1.0] {
                painter.circle_filled(
                    middle + along * (step * 4.5),
                    1.1,
                    palette.muted.gamma_multiply(0.7),
                );
            }
        }
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
    /// The keyboard modifiers winit last reported.
    window_modifiers: egui_winit::winit::keyboard::ModifiersState,
    /// Cmd+V was pressed since the last frame; see [`object_shortcuts`].
    paste_key_pressed: bool,
    #[cfg(feature = "dock-shell")]
    dock: dock::Host,
}

struct NewProjectDialog {
    name: String,
    error: Option<String>,
    language: engine_project::GameplayLanguage,
    template: engine_project::ProjectTemplate,
}

impl Default for NewProjectDialog {
    fn default() -> Self {
        Self {
            name: String::new(),
            error: None,
            language: engine_project::GameplayLanguage::Lua,
            template: engine_project::ProjectTemplate::Blank,
        }
    }
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
            window_modifiers: Default::default(),
            paste_key_pressed: false,
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
        match event {
            WindowEvent::ModifiersChanged(modifiers) => self.window_modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                self.paste_key_pressed |= is_paste_key(event, self.window_modifiers);
            }
            _ => {}
        }
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
                        .fill(crate::theme::NIGHT.shell)
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
                        .fill(crate::theme::NIGHT.shell)
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
                        state.pending_action.is_some() || state.sdk_upgrade_message.is_some(),
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

                    egui::CentralPanel::default()
                        .frame(studio_panel_frame())
                        .show(ui, |ui| show_scene_viewport(ui, viewport, state));
                }
            }
            unsaved_changes_modal(ui, state);
            new_project_modal(ui, state, &mut self.new_project);
            sdk_upgrade_message_modal(ui, state);
            crate::settings_window::show(ui.ctx(), state);
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

        // Cmd+, opens Settings, as in every Mac app.
        if self
            .context
            .input(|input| input.modifiers.command && input.key_pressed(egui::Key::Comma))
        {
            MenuCommand::OpenSettings.apply(state);
        }

        // Cmd+P plays in the Scene View (or stops), Cmd+Shift+P pauses or
        // resumes, Cmd+Option+P steps one frame. They work while the game
        // has focus too, so the preview can always be paused or stopped.
        let play_key = self.context.input(|input| {
            (input.modifiers.command && input.key_pressed(egui::Key::P))
                .then_some((input.modifiers.shift, input.modifiers.alt))
        });
        match play_key {
            Some((false, false)) => {
                MenuCommand::TogglePreview.apply(state);
            }
            Some((true, false)) => {
                MenuCommand::TogglePause.apply(state);
            }
            Some((false, true)) => {
                MenuCommand::StepFrame.apply(state);
            }
            _ => {}
        }

        // Cmd+G groups the selection, Cmd+Shift+G ungroups it.
        let group_pressed = !state.play.is_playing()
            && !self.context.text_edit_focused()
            && self
                .context
                .input(|input| input.modifiers.command && input.key_pressed(egui::Key::G));
        if group_pressed {
            if self.context.input(|input| input.modifiers.shift) {
                MenuCommand::Ungroup.apply(state);
            } else {
                MenuCommand::GroupSelection.apply(state);
            }
        }

        delete_shortcut(&self.context, state);
        object_shortcuts(
            &self.context,
            state,
            std::mem::take(&mut self.paste_key_pressed),
        );

        // Cmd+S saves (see `save_shortcut`).
        if self
            .context
            .input(|input| input.modifiers.command && input.key_pressed(SAVE_KEY))
        {
            save_shortcut(state);
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
            // Compared with the saved file, so an edit that puts things
            // back the way they were leaves the scene clean.
            state.sync_scene_dirty();
        }

        // After undo and redo, which rebuild the world and drop the markers.
        hierarchy::sync_editor_hidden(&mut state.world, &state.hierarchy.hidden);

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
            ui.colored_label(crate::theme::NIGHT.subtle, "STUDIO");
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
        crate::theme::NIGHT.raised
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
        crate::theme::NIGHT.accent
    } else {
        crate::theme::NIGHT.subtle
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
    let bottom_row = egui::Panel::bottom("scene_bottom_row")
        .resizable(true)
        .drag_to_open(false)
        .default_size(bottom_height)
        .min_size(SCENE_BOTTOM_MIN)
        .max_size(bottom_limit)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .show_studio_collapsible_bare(ui, &mut bottom_open, |ui| {
            let mut split_width = assets_width;
            let mut split = None;
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
                split = Some((response.clone(), row));
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
            // The cards in the row, so the row's top grip sits over each
            // card rather than across both as if they were one panel.
            let mut cards = Vec::new();
            if state.panels.asset_browser && state.panels.console {
                let assets = egui::Panel::left("scene_assets_panel")
                    .resizable(false)
                    .exact_size(split_width)
                    .frame(studio_panel_frame())
                    .show_separator_line(false)
                    .show_studio(ui, |ui| show_asset_browser(ui, state));
                let console = egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| bottom_tabs_panel(ui, state));
                cards.extend([assets.response.rect, console.response.rect]);
                if let Some((handle, row)) = &split {
                    let x = row.left() + split_width;
                    paint_grips(ui, &[ResizeEdge::Vertical(x, row.y_range())], Some(handle));
                }
            } else if state.panels.asset_browser {
                let shown = egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| show_asset_browser(ui, state));
                cards.push(shown.response.rect);
            } else if state.panels.console {
                let shown = egui::CentralPanel::default()
                    .frame(studio_panel_frame())
                    .show(ui, |ui| bottom_tabs_panel(ui, state));
                cards.push(shown.response.rect);
            }
            cards
        });
    if let Some(row) = &bottom_row {
        let top = row.response.rect.top();
        let edges: Vec<_> = row
            .inner
            .iter()
            .map(|card| ResizeEdge::Horizontal(top, card.x_range()))
            .collect();
        paint_grips(ui, &edges, None);
    }
    if !bottom_open {
        state.panels.asset_browser = false;
        state.panels.console = false;
    }
}

/// While a game plays in the Scene View the scene is read-only: the game
/// runs from the saved file, so edits could not reach it.
fn lock_while_previewing(ui: &mut egui::Ui, state: &EditorState) {
    if state.play.is_previewing() {
        ui.disable();
    }
}

fn show_hierarchy(ui: &mut egui::Ui, state: &mut EditorState) {
    lock_while_previewing(ui, state);
    state.hierarchy.image_selected = state.selected_texture_asset().is_some();
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
    if let Some((command, parent)) = state.hierarchy.create_request.take() {
        let before = state.selected_entity;
        command.apply(state);
        if let (Some(parent), Some(new)) = (parent, state.selected_entity)
            && state.selected_entity != before
        {
            hierarchy::adopt_new_object(&mut state.world, new, parent);
        }
    }
    if let Some((path, parent)) = state.hierarchy.asset_drop_request.take() {
        state.drop_asset(&path, None, parent);
    }
    if std::mem::take(&mut state.hierarchy.frame_request) {
        state.frame_selection();
        ui.ctx().request_repaint();
    }
}

/// Narrower than this, Project Assets stacks its preview under the file
/// tree instead of beside it, so file names are not cut short.
const PREVIEW_BESIDE_MIN_WIDTH: f32 = 420.0;

/// Whether a Project Assets panel `width` points wide shows its preview
/// under the tree (narrow) rather than beside it (wide).
fn preview_below_tree(width: f32) -> bool {
    width < PREVIEW_BESIDE_MIN_WIDTH
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
        ui.label(crate::theme::semibold("Project Assets"));
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
        let preview_body = |ui: &mut egui::Ui| {
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
        };
        let panel_width = ui.available_width();
        if preview_below_tree(panel_width) {
            // A narrow panel: the tree keeps the full width and the
            // preview sits under it.
            let panel_height = ui.available_height();
            egui::Panel::bottom("studio_asset_preview_below")
                .resizable(true)
                .default_size((panel_height * 0.4).clamp(90.0, 200.0))
                .min_size(70.0)
                .max_size((panel_height - 110.0).max(70.0))
                .frame(studio_panel_frame())
                .show_separator_line(false)
                .show_studio_collapsible(ui, &mut state.asset_browser.preview_open, preview_body);
        } else {
            egui::Panel::right("studio_asset_preview")
                .resizable(true)
                .default_size((panel_width * 0.45).clamp(80.0, 220.0))
                .min_size(80.0)
                .max_size((panel_width - 100.0).max(80.0))
                .frame(studio_panel_frame())
                .show_separator_line(false)
                .show_studio_collapsible(ui, &mut state.asset_browser.preview_open, preview_body);
        }
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
    if state.play.is_previewing() {
        crate::game_view::show(ui, state);
        return;
    }
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
    handle_scene_view_input(ui, viewport, state, &image_response);
    paint_scene_ui_preview(ui, state, image_response.rect);
    if let Some(message) = &state.game_view.error {
        crate::game_view::paint_error(ui, image_response.rect, message);
    }
}

/// A file from Project Assets over the Scene View: shows where it would land,
/// and adds it there on release. Scene edits wait until the game stops.
fn handle_asset_drop(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &mut EditorState,
    response: &egui::Response,
) {
    if state.play.is_playing() {
        return;
    }
    let released = response.dnd_release_payload::<std::path::PathBuf>();
    let Some(path) = released
        .clone()
        .or_else(|| response.dnd_hover_payload::<std::path::PathBuf>())
    else {
        return;
    };
    let Some(pointer) = ui.input(|input| input.pointer.latest_pos()) else {
        return;
    };
    let point = drop_point(viewport, state, response.rect, pointer);
    if released.is_some() {
        state.drop_asset(&path, Some(point), None);
        return;
    }
    let rect = response.rect;
    if let Some(at) = crate::gizmo::project_to_screen(
        viewport.view_projection_matrix(),
        point,
        Vec2::new(rect.width(), rect.height()),
    ) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        paint_drop_marker(ui, rect, rect.min + egui::vec2(at.x, at.y), &name);
    }
}

/// The world point a file dropped at `pointer` lands on: in 3D, whatever is
/// under the cursor, else the ground, else in front of the camera; in 2D, the
/// cursor on the 2D plane.
fn drop_point(
    viewport: &Viewport,
    state: &mut EditorState,
    rect: egui::Rect,
    pointer: egui::Pos2,
) -> glam::Vec3 {
    let local = Vec2::new(pointer.x - rect.min.x, pointer.y - rect.min.y);
    let ndc = Vec2::new(
        local.x / rect.width().max(1.0) * 2.0 - 1.0,
        1.0 - local.y / rect.height().max(1.0) * 2.0,
    );
    if state.dimension.is_2d() {
        let aspect = rect.width() / rect.height().max(1.0);
        let mut point = state.scene_camera().point_under(ndc, true, aspect);
        point.z = 0.0;
        return point;
    }
    let view_proj = viewport.view_projection_matrix();
    let hit = pick_scene(
        state,
        view_proj,
        rect,
        local,
        crate::scene_pick::LockedObjects::Include,
    )
    .map(|hit| hit.point);
    let camera = state.scene_camera();
    match crate::scene_pick::ray_from_screen(view_proj, ndc) {
        Some(ray) => crate::scene_pick::placement_point(hit, ray, camera.distance),
        None => hit.unwrap_or(camera.focus),
    }
}

/// Where a dragged file will land: a ring and dot at `at`, and its name.
fn paint_drop_marker(ui: &egui::Ui, clip: egui::Rect, at: egui::Pos2, name: &str) {
    let palette = crate::theme::NIGHT;
    let painter = ui.painter().with_clip_rect(clip);
    painter.circle_filled(at, 10.0, palette.accent.gamma_multiply(0.25));
    painter.circle_stroke(at, 10.0, egui::Stroke::new(1.5, palette.accent));
    painter.circle_filled(at, 2.5, palette.accent);
    let galley = painter.layout_no_wrap(
        name.to_owned(),
        egui::TextStyle::Small.resolve(ui.style()),
        palette.text,
    );
    let pill = egui::Rect::from_min_size(
        at + egui::vec2(16.0, -galley.size().y * 0.5 - 3.0),
        galley.size() + egui::vec2(12.0, 6.0),
    );
    painter.rect_filled(pill, 6.0, palette.accent_fill);
    painter.galley(pill.min + egui::vec2(6.0, 3.0), galley, palette.text);
}

/// Everything a Scene View image responds to: camera navigation first; when
/// the camera does not own the drag, the gizmo or active brush; then asset
/// drops and the cursor.
fn handle_scene_view_input(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &mut EditorState,
    image_response: &egui::Response,
) {
    handle_view_tool_keys(ui, state, image_response);
    let camera_gesture = handle_camera_navigation(ui, state, image_response);
    let icons = crate::scene_icons::project(
        &crate::scene_icons::collect(&mut state.world),
        viewport.view_projection_matrix(),
        Vec2::new(image_response.rect.width(), image_response.rect.height()),
    );
    if camera_gesture.is_none() {
        handle_scene_interaction(viewport, state, image_response);
        select_clicked_object(ui, viewport, state, image_response, &icons);
        focus_double_clicked_point(ui, viewport, state, image_response);
    }
    paint_scene_icons(ui, state, image_response.rect, &icons);
    let gizmo_hover = paint_gizmo(ui, viewport, state, image_response);
    handle_asset_drop(ui, viewport, state, image_response);
    let cursor = match camera_gesture {
        Some(CameraGesture::Pan) => egui::CursorIcon::AllScroll,
        Some(CameraGesture::Orbit | CameraGesture::Look) => egui::CursorIcon::Grabbing,
        None if image_response.hovered() && state.active_view_tool() == ViewTool::Hand => {
            egui::CursorIcon::Grab
        }
        None => scene_cursor_icon(image_response.dragged(), state.dragging_handle, gizmo_hover),
    };
    if scene_owns_cursor(
        image_response.hovered(),
        image_response.dragged(),
        camera_gesture.is_some(),
        state.dragging_handle.is_some(),
    ) {
        ui.ctx().set_cursor_icon(cursor);
    }
}

/// Whether the Scene View decides the mouse cursor this frame: while the
/// pointer is over it, or a drag that started in it is still going. Any
/// other time the cursor belongs to whatever is under the pointer. The
/// Scene View is drawn after the side panels, so setting it anyway would
/// undo their cursors, such as the Inspector's drag cursor.
fn scene_owns_cursor(hovered: bool, dragged: bool, camera_gesture: bool, gizmo_drag: bool) -> bool {
    hovered || dragged || camera_gesture || gizmo_drag
}

/// What a Scene View drag does to the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraGesture {
    /// Rotate around the focus point.
    Orbit,
    /// Turn in place while flying with WASD.
    Look,
    /// Slide the view.
    Pan,
}

/// Which mouse buttons are dragging over the Scene View this frame.
#[derive(Debug, Clone, Copy, Default)]
struct DragButtons {
    primary: bool,
    secondary: bool,
    middle: bool,
}

/// Maps buttons and modifiers to a camera gesture, following common editor
/// conventions: right-drag orbits (looks around while flying), middle-drag or
/// Shift+right-drag pans, and ⌘+left-drag orbits (for trackpads; a plain
/// ⌘-click still adds to the selection). 2D views only pan. A plain left-drag
/// is left to the gizmo and brushes; ⌥ (Option/Alt) has no camera role.
///
/// With the Hand [`ViewTool`] a plain left-drag pans instead.
fn camera_gesture(
    buttons: DragButtons,
    modifiers: egui::Modifiers,
    two_d: bool,
    flying: bool,
    tool: ViewTool,
) -> Option<CameraGesture> {
    let tool_gesture = match tool {
        ViewTool::Select => None,
        ViewTool::Hand => Some(CameraGesture::Pan),
    };
    let gesture = if buttons.middle || (buttons.secondary && modifiers.shift) {
        CameraGesture::Pan
    } else if buttons.secondary && flying {
        CameraGesture::Look
    } else if buttons.secondary || (buttons.primary && modifiers.command) {
        CameraGesture::Orbit
    } else if buttons.primary
        && let Some(gesture) = tool_gesture
    {
        gesture
    } else {
        return None;
    };
    // The 2D view does not rotate: turning gestures pan instead.
    Some(match gesture {
        CameraGesture::Orbit | CameraGesture::Look if two_d => CameraGesture::Pan,
        other => other,
    })
}

/// WASD/QE movement while the right mouse button is held: (right, up, forward).
fn fly_direction(input: &egui::InputState) -> glam::Vec3 {
    let axis = |positive, negative| {
        f32::from(u8::from(input.key_down(positive)))
            - f32::from(u8::from(input.key_down(negative)))
    };
    glam::Vec3::new(
        axis(egui::Key::D, egui::Key::A),
        axis(egui::Key::E, egui::Key::Q),
        axis(egui::Key::W, egui::Key::S),
    )
}

/// Tool keys while the pointer is over the Scene View (and no text field has
/// focus): **V** Select, **H** Hand, and holding **Space** gives Hand until
/// it is released. Fly keys belong to right-drag, so these never clash.
fn handle_view_tool_keys(ui: &egui::Ui, state: &mut EditorState, response: &egui::Response) {
    let typing = ui.ctx().text_edit_focused();
    let flying = response.dragged_by(egui::PointerButton::Secondary);
    let (select, hand, space) = ui.input(|input| {
        (
            input.key_pressed(egui::Key::V),
            input.key_pressed(egui::Key::H),
            input.key_down(egui::Key::Space),
        )
    });
    let over = response.hovered() || response.dragged();
    if !typing && !flying && over {
        if select {
            state.view_tool = ViewTool::Select;
        } else if hand {
            state.view_tool = ViewTool::Hand;
        }
    }
    let held = !typing && space && (over || state.temporary_hand);
    if held != state.temporary_hand {
        state.temporary_hand = held;
        ui.ctx().request_repaint();
    }
}

/// Orbit, pan, zoom, fly and frame-selected for the Scene View camera.
/// Returns the drag gesture in progress, if the camera owns this drag.
fn handle_camera_navigation(
    ui: &egui::Ui,
    state: &mut EditorState,
    response: &egui::Response,
) -> Option<CameraGesture> {
    let two_d = state.dimension.is_2d();
    let buttons = DragButtons {
        primary: response.dragged_by(egui::PointerButton::Primary),
        secondary: response.dragged_by(egui::PointerButton::Secondary),
        middle: response.dragged_by(egui::PointerButton::Middle),
    };
    let typing = ui.ctx().text_edit_focused();
    let (modifiers, dt, fly, scroll, pointer, frame) = ui.input(|input| {
        let fly = if !two_d && buttons.secondary && !typing {
            fly_direction(input)
        } else {
            glam::Vec3::ZERO
        };
        (
            input.modifiers,
            input.stable_dt,
            fly,
            scroll_intent(&input.events),
            input.pointer.hover_pos(),
            !typing && input.key_pressed(egui::Key::F),
        )
    });
    let gesture = camera_gesture(
        buttons,
        modifiers,
        two_d,
        fly != glam::Vec3::ZERO,
        state.active_view_tool(),
    );
    let rect = response.rect;
    let drag = response.drag_delta();
    let camera = state.scene_cameras.active_mut(two_d);
    let before = *camera;
    match gesture {
        Some(CameraGesture::Orbit) => camera.orbit(Vec2::new(drag.x, drag.y)),
        Some(CameraGesture::Look) => camera.look(Vec2::new(drag.x, drag.y)),
        Some(CameraGesture::Pan) => camera.pan(Vec2::new(drag.x, drag.y), two_d, rect.height()),
        None => {}
    }
    if fly != glam::Vec3::ZERO {
        camera.fly(fly, dt, modifiers.shift);
    }
    if response.hovered() {
        if scroll.pan != Vec2::ZERO {
            camera.pan(scroll.pan, two_d, rect.height());
        }
        if scroll.orbit != Vec2::ZERO {
            // The 2D view has no rotation; there the gesture pans instead.
            if two_d {
                camera.pan(scroll.orbit, two_d, rect.height());
            } else {
                camera.orbit(scroll.orbit);
            }
        }
        let steps = scroll.zoom_steps;
        if steps.abs() > f32::EPSILON {
            let toward = pointer.map(|pointer| {
                let ndc = Vec2::new(
                    (pointer.x - rect.center().x) / (rect.width() * 0.5).max(1.0),
                    (rect.center().y - pointer.y) / (rect.height() * 0.5).max(1.0),
                );
                camera.point_under(ndc, two_d, rect.width() / rect.height().max(1.0))
            });
            camera.zoom(steps, two_d, toward);
        }
    }
    let moved = *camera != before;
    if moved {
        // The user took control; a glide in progress would fight them.
        state.scene_cameras.stop_glide(two_d);
    }
    if response.hovered() && frame {
        state.frame_selection();
    }
    let gliding = state.scene_cameras.advance_glide(two_d, dt);
    if moved || frame || gliding || fly != glam::Vec3::ZERO {
        ui.ctx().request_repaint();
    }
    gesture
}

/// Scroll input this frame, split by device.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ScrollIntent {
    /// Zoom steps; positive zooms in.
    zoom_steps: f32,
    /// Pan distance in points, moving the scene with the fingers.
    pan: Vec2,
    /// Orbit distance in points (⌘ + two-finger scroll).
    orbit: Vec2,
}

/// Mouse-wheel notches (line units) and pinches zoom. Trackpad two-finger
/// scrolling (point units) pans, as on macOS; with ⌘/Ctrl held it orbits.
fn scroll_intent(events: &[egui::Event]) -> ScrollIntent {
    // One zoom step per wheel notch. A pinch reports small scale factors, so
    // it is amplified to feel as quick as scrolling did.
    const PINCH_GAIN: f32 = 2.5;
    let pinch_step = (1.0f32 / 0.88).ln();
    let mut intent = ScrollIntent::default();
    for event in events {
        match event {
            egui::Event::Zoom(factor) if factor.is_finite() && *factor > 0.0 => {
                intent.zoom_steps += factor.ln() / pinch_step * PINCH_GAIN;
            }
            egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => match unit {
                egui::MouseWheelUnit::Line => intent.zoom_steps += delta.y,
                egui::MouseWheelUnit::Page => intent.zoom_steps += delta.y * 3.0,
                egui::MouseWheelUnit::Point if modifiers.command || modifiers.ctrl => {
                    intent.orbit += Vec2::new(delta.x, delta.y);
                }
                egui::MouseWheelUnit::Point => intent.pan += Vec2::new(delta.x, delta.y),
            },
            _ => {}
        }
    }
    intent
}

/// Cursor contract for the Scene image. The image accepts clicks and drags
/// for gizmos, but must not advertise a grab operation while idle. A rotation
/// ring or scale knob hovered or dragged hides the system cursor: egui has
/// no rotate or scale cursor, so [`paint_gizmo`] draws one in its place, and
/// scaling never looks like moving. Keeping this decision in one
/// pure helper makes the interaction regression-testable without creating a
/// GPU window in CI.
fn scene_cursor_icon(
    image_dragged: bool,
    dragging: Option<GizmoHandle>,
    hovered: Option<GizmoHandle>,
) -> egui::CursorIcon {
    if matches!(
        dragging.or(hovered),
        Some(GizmoHandle::Rotate(_) | GizmoHandle::ScaleAxis(_) | GizmoHandle::Scale)
    ) {
        egui::CursorIcon::None
    } else if image_dragged && dragging.is_some() {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Default
    }
}

fn ui_preview_rect(screen: egui::Rect, data: &engine_scene::UiElementData) -> egui::Rect {
    use engine_scene::UiAnchor;
    let (x, y) = match data.anchor {
        UiAnchor::TopLeft => (0.0, 0.0),
        UiAnchor::Top => (0.5, 0.0),
        UiAnchor::TopRight => (1.0, 0.0),
        UiAnchor::Left => (0.0, 0.5),
        UiAnchor::Center => (0.5, 0.5),
        UiAnchor::Right => (1.0, 0.5),
        UiAnchor::BottomLeft => (0.0, 1.0),
        UiAnchor::Bottom => (0.5, 1.0),
        UiAnchor::BottomRight => (1.0, 1.0),
    };
    let size = egui::vec2(data.size[0], data.size[1]);
    let point = screen.min
        + egui::vec2(screen.width() * x, screen.height() * y)
        + egui::vec2(data.offset[0], data.offset[1]);
    egui::Rect::from_min_size(point - egui::vec2(size.x * x, size.y * y), size)
}

fn paint_scene_ui_preview(ui: &egui::Ui, state: &mut EditorState, screen: egui::Rect) {
    use engine_ecs::components::{Disabled, Hidden};
    use engine_ecs::prelude::{Entity, Without};
    use engine_scene::{SceneUi, UiKind};

    let mut query = state
        .world
        .query_filtered::<(Entity, &SceneUi, Option<&Disabled>), Without<Hidden>>();
    let elements: Vec<_> = query
        .iter(&state.world)
        .filter(|(_, _, disabled)| disabled.is_none())
        .map(|(entity, data, _)| (entity, data.0.clone()))
        .collect();
    let painter = ui.painter().with_clip_rect(screen);
    for (entity, data) in elements {
        let rect = ui_preview_rect(screen, &data);
        if !rect.intersects(screen) {
            continue;
        }
        let color = egui::Color32::from_rgba_unmultiplied(
            (data.color[0] * 255.0) as u8,
            (data.color[1] * 255.0) as u8,
            (data.color[2] * 255.0) as u8,
            (data.color[3] * 255.0) as u8,
        );
        if data.kind == UiKind::Button {
            painter.rect_filled(rect, 5.0, color);
        }
        painter.text(
            rect.left_center() + egui::vec2(7.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &data.text,
            egui::FontId::proportional(14.0),
            if data.kind == UiKind::Label {
                color
            } else {
                egui::Color32::WHITE
            },
        );
        if state.selected_entity == Some(entity) {
            painter.rect_stroke(
                rect,
                5.0,
                egui::Stroke::new(1.0, crate::theme::NIGHT.accent),
                egui::StrokeKind::Inside,
            );
        }
    }
}

fn handle_scene_interaction(
    viewport: &Viewport,
    state: &mut EditorState,
    image_response: &egui::Response,
) {
    if state.tile_paint_active
        && state.dimension.is_2d()
        && state.selected_entity.is_some_and(|entity| {
            state
                .world
                .get::<engine_scene::SceneTilemap>(entity)
                .is_some()
        })
    {
        state.dragging_handle = None;
        handle_tilemap_interaction(viewport, state, image_response);
    } else if state.terrain_sculpt_active {
        state.dragging_handle = None;
        handle_terrain_interaction(viewport, state, image_response);
    } else {
        handle_gizmo_interaction(viewport, state, image_response);
    }
}

fn handle_tilemap_interaction(
    viewport: &Viewport,
    state: &mut EditorState,
    response: &egui::Response,
) {
    if !(response.clicked() || response.drag_started() || response.dragged()) {
        return;
    }
    let Some(parent) = state.selected_entity else {
        return;
    };
    let Some(map) = state.world.get::<engine_scene::SceneTilemap>(parent) else {
        return;
    };
    let Some(pointer) = response.interact_pointer_pos() else {
        return;
    };
    let local = Vec2::new(
        pointer.x - response.rect.min.x,
        pointer.y - response.rect.min.y,
    );
    let size = Vec2::new(response.rect.width(), response.rect.height());
    let model = hierarchy::world_transform(&state.world, parent).to_matrix();
    let Some((x, y)) = crate::tilemap::cell_from_screen(
        viewport.view_projection_matrix(),
        model,
        local,
        size,
        map.0.cell_size,
    ) else {
        return;
    };
    match state.paint_tile_cell(x, y, state.tile_erase_active) {
        Ok(true) => response.ctx.request_repaint(),
        Ok(false) => {}
        Err(error) => state.asset_browser.operation_error = Some(error),
    }
}

fn handle_terrain_interaction(
    viewport: &Viewport,
    state: &mut EditorState,
    response: &egui::Response,
) {
    if response.drag_started() {
        match state.begin_terrain_stroke() {
            Ok(stroke) => state.terrain_stroke = Some(stroke),
            Err(error) => state.asset_browser.operation_error = Some(error),
        }
    }
    if (response.drag_started() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(stroke) = state.terrain_stroke.as_mut()
        && state.selected_entity == Some(stroke.entity)
    {
        let local = Vec2::new(
            pointer.x - response.rect.min.x,
            pointer.y - response.rect.min.y,
        );
        let size = Vec2::new(response.rect.width(), response.rect.height());
        let model = hierarchy::world_transform(&state.world, stroke.entity).to_matrix();
        if let Some(hit) = crate::terrain_tools::terrain_hit_from_screen(
            viewport.view_projection_matrix(),
            model,
            &stroke.heightmap,
            local,
            size,
        ) {
            stroke.record(hit, (state.terrain_tools.radius * 0.5).max(0.01));
        }
    }
    if response.drag_stopped() {
        state.finish_terrain_stroke();
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
/// Click-to-select in the Scene View, in priority order: an icon, then a
/// gizmo handle (which keeps the click), then the nearest mesh or sprite under
/// the pointer. Clicking empty space clears the selection. Cmd/Ctrl-click
/// adds to or removes from the selection. Brush tools keep their clicks.
fn select_clicked_object(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &mut EditorState,
    response: &egui::Response,
    icons: &[(crate::scene_icons::SceneIcon, Vec2)],
) {
    let brushing = state.terrain_sculpt_active || state.tile_paint_active;
    let navigating = state.active_view_tool() != ViewTool::Select;
    if brushing || navigating || !response.clicked() || state.dragging_handle.is_some() {
        return;
    }
    let Some(pointer) = response.interact_pointer_pos() else {
        return;
    };
    let rect = response.rect;
    let local = Vec2::new(pointer.x - rect.min.x, pointer.y - rect.min.y);
    let view_proj = viewport.view_projection_matrix();
    let on_gizmo = current_gizmo(viewport, state, rect).is_some_and(|layout| {
        let rings = gizmo::rings_revealed(&layout, Some(local), state.dragging_handle);
        gizmo::pick(&layout, local, rings).is_some()
    });
    if on_gizmo {
        return;
    }
    let picked = crate::scene_icons::pick(icons, local).or_else(|| {
        pick_scene(
            state,
            view_proj,
            rect,
            local,
            crate::scene_pick::LockedObjects::Skip,
        )
        .map(|hit| hit.entity)
    });
    let additive = ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
    match (picked, additive) {
        (Some(entity), true) => hierarchy::select_toggle(
            &mut state.selected_entity,
            &mut state.secondary_selection,
            entity,
        ),
        (Some(entity), false) => state.select_only(entity),
        (None, false) => state.clear_selection(),
        (None, true) => {}
    }
    ui.ctx().request_repaint();
}

/// The object and surface point under viewport-local `local`, tested against
/// imported mesh triangles and sprite quads.
fn pick_scene(
    state: &mut EditorState,
    view_proj: glam::Mat4,
    rect: egui::Rect,
    local: Vec2,
    locked: crate::scene_pick::LockedObjects,
) -> Option<crate::scene_pick::PickHit> {
    let ndc = Vec2::new(
        local.x / rect.width().max(1.0) * 2.0 - 1.0,
        1.0 - local.y / rect.height().max(1.0) * 2.0,
    );
    let ray = crate::scene_pick::ray_from_screen(view_proj, ndc)?;
    let importer = &state.importer;
    crate::scene_pick::pick_hit(
        &mut state.world,
        ray,
        |source| {
            let id = uuid::Uuid::parse_str(&source.mesh).ok()?;
            match importer.get(engine_asset::AssetId::from_uuid(id))? {
                crate::ImportedAsset::Mesh(gltf) => gltf
                    .meshes
                    .first()
                    .map(|mesh| (mesh.vertices.as_slice(), mesh.indices.as_slice())),
                _ => None,
            }
        },
        locked,
    )
}

/// Double-click a surface to glide the camera to look at that spot. Locked
/// objects (such as a locked terrain) still count as surfaces here.
fn focus_double_clicked_point(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &mut EditorState,
    response: &egui::Response,
) {
    let brushing = state.terrain_sculpt_active || state.tile_paint_active;
    if brushing || !response.double_clicked() {
        return;
    }
    let Some(pointer) = response.interact_pointer_pos() else {
        return;
    };
    let rect = response.rect;
    let local = Vec2::new(pointer.x - rect.min.x, pointer.y - rect.min.y);
    let Some(hit) = pick_scene(
        state,
        viewport.view_projection_matrix(),
        rect,
        local,
        crate::scene_pick::LockedObjects::Include,
    ) else {
        return;
    };
    let two_d = state.dimension.is_2d();
    let mut target = *state.scene_camera();
    target.focus_on(hit.point);
    state.scene_cameras.glide_to(two_d, target);
    ui.ctx().request_repaint();
}

/// Paints the icons for cameras, lights and sounds over the Scene View.
fn paint_scene_icons(
    ui: &egui::Ui,
    state: &EditorState,
    rect: egui::Rect,
    icons: &[(crate::scene_icons::SceneIcon, Vec2)],
) {
    let painter = ui.painter().with_clip_rect(rect);
    for (icon, at) in icons {
        let selected = state.selected_entity == Some(icon.entity)
            || state.secondary_selection.contains(&icon.entity);
        crate::scene_icons::paint(
            &painter,
            icon.kind,
            rect.min + egui::vec2(at.x, at.y),
            selected,
        );
    }
}

/// A gizmo colour as an egui colour: faint until hovered (rings especially,
/// so the object stays visible), bright when hovered, yellow while dragged.
fn gizmo_color(axis: Axis, emphasis: GizmoEmphasis, faint: bool) -> egui::Color32 {
    let [r, g, b, _] = axis.color();
    let channel = |value: f32, lift: f32| ((value + lift).min(1.0) * 255.0) as u8;
    match emphasis {
        GizmoEmphasis::Active => crate::theme::NIGHT.highlight,
        GizmoEmphasis::Hovered => {
            egui::Color32::from_rgb(channel(r, 0.25), channel(g, 0.25), channel(b, 0.25))
        }
        GizmoEmphasis::Normal => {
            let color = egui::Color32::from_rgb(channel(r, 0.0), channel(g, 0.0), channel(b, 0.0));
            if faint {
                color.gamma_multiply(0.55)
            } else {
                color
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GizmoEmphasis {
    Normal,
    Hovered,
    Active,
}

/// The combined gizmo for the current selection, in viewport-local pixels;
/// `None` when nothing movable is selected or a tool owns the viewport.
fn current_gizmo(
    viewport: &Viewport,
    state: &EditorState,
    rect: egui::Rect,
) -> Option<gizmo::GizmoLayout> {
    let painting_tiles = state.tile_paint_active && state.dimension.is_2d();
    let navigating = state.active_view_tool() != ViewTool::Select;
    if state.terrain_sculpt_active || painting_tiles || navigating {
        return None;
    }
    gizmo::layout(
        viewport.view_projection_matrix(),
        state.gizmo_origin()?,
        state.dimension.is_2d(),
        Vec2::new(rect.width(), rect.height()),
    )
}

/// Paints the combined gizmo over the Scene View image, always on top:
/// faint rotation rings, move arrows with their scale knobs, and the centre
/// scale square. The handle under the pointer brightens; the one being
/// dragged turns yellow. Returns the hovered handle, for the cursor.
fn paint_gizmo(
    ui: &egui::Ui,
    viewport: &Viewport,
    state: &EditorState,
    response: &egui::Response,
) -> Option<GizmoHandle> {
    let rect = response.rect;
    let layout = current_gizmo(viewport, state, rect)?;
    let to_screen = |point: Vec2| rect.min + egui::vec2(point.x, point.y);
    let pointer = response
        .hover_pos()
        .map(|pointer| Vec2::new(pointer.x - rect.min.x, pointer.y - rect.min.y));
    let rings_shown = gizmo::rings_revealed(&layout, pointer, state.dragging_handle);
    let hovered = pointer
        .filter(|_| state.dragging_handle.is_none())
        .and_then(|pointer| gizmo::pick(&layout, pointer, rings_shown));
    let emphasis = |handle: GizmoHandle| {
        if state.dragging_handle == Some(handle) {
            GizmoEmphasis::Active
        } else if hovered == Some(handle) {
            GizmoEmphasis::Hovered
        } else {
            GizmoEmphasis::Normal
        }
    };
    let painter = ui.painter().with_clip_rect(rect);
    for ring in layout.rings.iter().filter(|_| rings_shown) {
        let level = emphasis(GizmoHandle::Rotate(ring.axis));
        let width = if level == GizmoEmphasis::Normal {
            1.5
        } else {
            3.0
        };
        let points = ring.points.iter().map(|point| to_screen(*point)).collect();
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(width, gizmo_color(ring.axis, level, true)),
        ));
    }
    for arrow in &layout.arrows {
        let level = emphasis(GizmoHandle::Move(arrow.axis));
        let color = gizmo_color(arrow.axis, level, false);
        let width = if level == GizmoEmphasis::Normal {
            3.0
        } else {
            4.0
        };
        let start = to_screen(arrow.start);
        let end = to_screen(arrow.end);
        let direction = (end - start).normalized();
        // The tip sits inside the arrow's length, so the whole shape is grabbable.
        let base = end - direction * 14.0;
        painter.line_segment([start, base], egui::Stroke::new(width, color));
        let side = egui::vec2(-direction.y, direction.x) * 7.0;
        painter.add(egui::Shape::convex_polygon(
            vec![end, base + side, base - side],
            color,
            egui::Stroke::NONE,
        ));
        let knob_level = emphasis(GizmoHandle::ScaleAxis(arrow.axis));
        let knob_size = if knob_level == GizmoEmphasis::Normal {
            gizmo::SCALE_KNOB_HALF_SIZE_PX * 2.0
        } else {
            gizmo::SCALE_KNOB_HALF_SIZE_PX * 2.0 + 2.0
        };
        let knob =
            egui::Rect::from_center_size(to_screen(arrow.knob()), egui::Vec2::splat(knob_size));
        painter.rect_filled(knob, 1.0, gizmo_color(arrow.axis, knob_level, false));
        painter.rect_stroke(
            knob,
            1.0,
            egui::Stroke::new(1.0, crate::theme::NIGHT.shell),
            egui::StrokeKind::Outside,
        );
    }
    let centre = to_screen(layout.origin);
    let square =
        egui::Rect::from_center_size(centre, egui::Vec2::splat(gizmo::SCALE_HALF_SIZE_PX * 2.0));
    let fill = match emphasis(GizmoHandle::Scale) {
        GizmoEmphasis::Active => crate::theme::NIGHT.highlight,
        GizmoEmphasis::Hovered => egui::Color32::WHITE,
        GizmoEmphasis::Normal => crate::theme::NIGHT.text,
    };
    painter.rect_filled(square, 1.5, fill);
    painter.rect_stroke(
        square,
        1.5,
        egui::Stroke::new(1.0, crate::theme::NIGHT.shell),
        egui::StrokeKind::Outside,
    );
    // egui has no rotate or scale cursor, so while a ring or a scale knob is
    // hovered or dragged the system cursor is hidden (see
    // `scene_cursor_icon`) and one is drawn at the pointer instead, as
    // Photoshop does. Moving keeps the system pointer.
    if let Some(pointer) = response.hover_pos().or(response.interact_pointer_pos()) {
        let painter = ui.ctx().layer_painter(cursor_layer());
        match state.dragging_handle.or(hovered) {
            Some(GizmoHandle::Rotate(_)) => paint_rotate_cursor(&painter, pointer),
            Some(GizmoHandle::ScaleAxis(_) | GizmoHandle::Scale) => {
                paint_scale_cursor(&painter, pointer);
            }
            _ => {}
        }
    }
    hovered
}

/// The topmost layer, so the drawn rotate cursor is never hidden by
/// anything else painted over the viewport.
fn cursor_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("scene_rotate_cursor"))
}

/// The "scale" cursor: a small square with a diagonal double-headed arrow
/// through it, pointing out from the square's corners, centred on `hotspot`;
/// white over a dark outline so it reads on any background.
fn paint_scale_cursor(painter: &egui::Painter, hotspot: egui::Pos2) {
    let square = egui::Rect::from_center_size(hotspot, egui::Vec2::splat(7.0));
    let (low, high) = (
        hotspot + egui::vec2(-10.0, 10.0),
        hotspot + egui::vec2(10.0, -10.0),
    );
    let heads = [
        [low, low + egui::vec2(0.0, -4.5)],
        [low, low + egui::vec2(4.5, 0.0)],
        [high, high + egui::vec2(0.0, 4.5)],
        [high, high + egui::vec2(-4.5, 0.0)],
    ];
    for stroke in [
        egui::Stroke::new(3.2, egui::Color32::from_black_alpha(160)),
        egui::Stroke::new(1.6, egui::Color32::WHITE),
    ] {
        painter.line_segment([low, square.left_bottom()], stroke);
        painter.line_segment([square.right_top(), high], stroke);
        for head in heads {
            painter.line_segment(head, stroke);
        }
        painter.rect_stroke(square, 1.0, stroke, egui::StrokeKind::Middle);
    }
}

/// The "rotate" cursor: a curved double-headed arrow around a hotspot dot at
/// `hotspot`, white over a dark outline so it reads on any background.
fn paint_rotate_cursor(painter: &egui::Painter, hotspot: egui::Pos2) {
    painter.circle_filled(hotspot, 2.6, egui::Color32::from_black_alpha(170));
    painter.circle_filled(hotspot, 1.4, egui::Color32::WHITE);
    let centre = hotspot + egui::vec2(0.0, 4.0);
    let arc: Vec<_> = (0..=12)
        .map(|step| {
            let angle = std::f32::consts::PI * (0.15 + 0.7 * step as f32 / 12.0);
            centre + egui::vec2(-9.0 * angle.cos(), -9.0 * angle.sin())
        })
        .collect();
    let (Some(&first), Some(&last)) = (arc.first(), arc.last()) else {
        return;
    };
    let heads = [
        [first, first + egui::vec2(3.0, -2.5)],
        [first, first + egui::vec2(-0.5, -3.5)],
        [last, last + egui::vec2(-3.0, -2.5)],
        [last, last + egui::vec2(0.5, -3.5)],
    ];
    for stroke in [
        egui::Stroke::new(3.2, egui::Color32::from_black_alpha(160)),
        egui::Stroke::new(1.6, egui::Color32::WHITE),
    ] {
        painter.add(egui::Shape::line(arc.clone(), stroke));
        for head in heads {
            painter.line_segment(head, stroke);
        }
    }
}

/// One frame's change from dragging a gizmo handle.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GizmoEdit {
    /// World units along an axis.
    Move(Axis, f32),
    /// Radians around an axis.
    Rotate(Axis, f32),
    /// Scale factor along an axis.
    ScaleAxis(Axis, f32),
    /// Uniform scale factor.
    Scale(f32),
}

impl GizmoEdit {
    fn apply(self, transform: engine_utils::Transform) -> engine_utils::Transform {
        match self {
            GizmoEdit::Move(axis, distance) => {
                gizmo::translate_transform(transform, axis, distance)
            }
            GizmoEdit::Rotate(axis, radians) => gizmo::rotate_transform(transform, axis, radians),
            GizmoEdit::ScaleAxis(axis, factor) => gizmo::scale_along(transform, axis, factor),
            GizmoEdit::Scale(factor) => gizmo::scale_uniform(transform, factor),
        }
    }
}

/// Applies one frame of a gizmo drag to every selected, unlocked object.
/// The gizmo's axes are world axes, so each edit is made to the object's
/// world transform and stored back relative to its parent. An object whose
/// group is also selected moves with the group, not twice.
fn apply_gizmo_edit(
    world: &mut engine_ecs::prelude::World,
    selection: &[engine_ecs::prelude::Entity],
    edit: GizmoEdit,
) {
    let unlocked: Vec<_> = selection
        .iter()
        .copied()
        .filter(|&entity| world.get::<engine_ecs::components::Lock>(entity).is_none())
        .filter(|&entity| inspector::transform_of(world, entity).is_some())
        .collect();
    for target in hierarchy::without_descendants(world, &unlocked) {
        let current = hierarchy::world_transform(world, target);
        hierarchy::set_world_transform(world, target, edit.apply(current));
    }
}

/// Where the drag reported by `response` began: the point the button was
/// pressed, not where the pointer is once egui counts the press as a drag.
fn drag_start_point(response: &egui::Response) -> Option<egui::Pos2> {
    response
        .ctx
        .input(|input| input.pointer.press_origin())
        .or_else(|| response.interact_pointer_pos())
}

/// Drags on the combined gizmo: an arrow moves, a ring turns like a dial,
/// an arrow's knob stretches along its axis, the centre square scales evenly. Every selected, unlocked object changes
/// together.
fn handle_gizmo_interaction(
    viewport: &Viewport,
    state: &mut EditorState,
    image_response: &egui::Response,
) {
    let rect = image_response.rect;
    let Some(layout) = current_gizmo(viewport, state, rect) else {
        state.dragging_handle = None;
        return;
    };
    let local = |pointer: egui::Pos2| Vec2::new(pointer.x - rect.min.x, pointer.y - rect.min.y);
    // Pick the handle where the button went down. egui reports a drag only
    // once the pointer has moved a few pixels, and by then it may have
    // slid off a small scale knob onto its arrow, turning a scale into a
    // move while the cursor still said "scale".
    if image_response.drag_started()
        && let Some(pointer) = drag_start_point(image_response)
    {
        let at = local(pointer);
        let rings = gizmo::rings_revealed(&layout, Some(at), None);
        state.dragging_handle = gizmo::pick(&layout, at, rings);
    }

    if image_response.dragged()
        && let Some(handle) = state.dragging_handle
        && let Some(pointer) = image_response.interact_pointer_pos()
    {
        let drag = image_response.drag_delta();
        let drag = Vec2::new(drag.x, drag.y);
        let current = local(pointer);
        let edit = match handle {
            GizmoHandle::Move(axis) => layout
                .arrows
                .iter()
                .find(|arrow| arrow.axis == axis)
                // Pixels along the arrow → world units at the object's depth,
                // so the object tracks the pointer.
                .map(|arrow| {
                    GizmoEdit::Move(axis, gizmo::drag_along(arrow, drag) * arrow.world_per_pixel)
                }),
            GizmoHandle::Rotate(axis) => {
                layout
                    .rings
                    .iter()
                    .find(|ring| ring.axis == axis)
                    .map(|ring| {
                        GizmoEdit::Rotate(
                            axis,
                            gizmo::dial_angle(layout.origin, current - drag, current, ring.facing),
                        )
                    })
            }
            GizmoHandle::ScaleAxis(axis) => layout
                .arrows
                .iter()
                .find(|arrow| arrow.axis == axis)
                .map(|arrow| {
                    GizmoEdit::ScaleAxis(
                        axis,
                        gizmo::drag_axis_scale_factor(gizmo::drag_along(arrow, drag)),
                    )
                }),
            GizmoHandle::Scale => Some(GizmoEdit::Scale(gizmo::drag_scale_factor(drag))),
        };
        if let Some(edit) = edit {
            let selection = state.selection();
            apply_gizmo_edit(&mut state.world, &selection, edit);
        }
    }

    if image_response.drag_stopped() {
        state.dragging_handle = None;
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
        if ui.button("Save Scene   ⌘S").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::SaveScene);
            ui.close();
        }
        if ui
            .add_enabled(
                state.project.language() == engine_project::GameplayLanguage::Rust
                    && !state.dirty.any()
                    && !state.play.is_playing()
                    && !state.export_running,
                egui::Button::new("Upgrade Rust SDK"),
            )
            .on_disabled_hover_text("Available for a saved, idle Rust project")
            .clicked()
        {
            dispatch_menu_command(state, new_project, MenuCommand::UpgradeRustSdk);
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
        if ui.button("Settings…   ⌘,").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::OpenSettings);
            ui.close();
        }
        ui.separator();
        if ui.button("Exit").clicked() {
            dispatch_menu_command(state, new_project, MenuCommand::Quit);
            ui.close();
        }
    });
}

fn sdk_upgrade_message_modal(ui: &mut egui::Ui, state: &mut EditorState) {
    let Some(message) = state.sdk_upgrade_message.clone() else {
        return;
    };
    let response = crate::theme::modal(ui.ctx(), egui::Id::new("studio_sdk_upgrade_modal"), |ui| {
        ui.set_width(380.0);
        ui.heading("Rust SDK upgrade");
        ui.label(message);
        if ui
            .add(egui::Button::new("OK").fill(crate::theme::NIGHT.accent_fill))
            .clicked()
        {
            state.sdk_upgrade_message = None;
        }
    });
    if response.should_close() {
        state.sdk_upgrade_message = None;
    }
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
        ui.add_space(8.0);
        ui.label("Gameplay language");
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut dialog_state.language,
                engine_project::GameplayLanguage::Lua,
                "Lua · recommended",
            );
            ui.selectable_value(
                &mut dialog_state.language,
                engine_project::GameplayLanguage::Rust,
                "Rust",
            );
        });
        ui.add_space(8.0);
        ui.label("Starter scene");
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut dialog_state.template,
                engine_project::ProjectTemplate::Blank,
                "Blank",
            );
            ui.selectable_value(
                &mut dialog_state.template,
                engine_project::ProjectTemplate::Starter2D,
                "2D starter",
            );
            ui.selectable_value(
                &mut dialog_state.template,
                engine_project::ProjectTemplate::Starter3D,
                "3D starter",
            );
        });
        if let Some(error) = &dialog_state.error {
            ui.colored_label(crate::theme::NIGHT.danger, error);
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let valid_name = !dialog_state.name.trim().is_empty();
            if ui
                .add_enabled(
                    valid_name,
                    egui::Button::new("Choose Location…").fill(crate::theme::NIGHT.accent_fill),
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
                        request_guarded(
                            state,
                            PendingAction::NewProject(
                                target,
                                dialog_state.language,
                                dialog_state.template,
                            ),
                        );
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

/// Delete or Backspace removes the selection (undo with Cmd+Z), but not
/// while typing — in a text field, or in the terminal, where Backspace
/// belongs to the shell — and not in Play.
fn delete_shortcut(ctx: &egui::Context, state: &mut EditorState) {
    if !state.play.is_playing()
        && !ctx.text_edit_focused()
        && !terminal_has_keyboard(ctx, state)
        && ctx.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        })
    {
        MenuCommand::DeleteSelected.apply(state);
    }
}

/// Whether `event` is the paste shortcut being pressed.
fn is_paste_key(
    event: &egui_winit::winit::event::KeyEvent,
    modifiers: egui_winit::winit::keyboard::ModifiersState,
) -> bool {
    use egui_winit::winit::keyboard::{Key, KeyCode, PhysicalKey};
    let command = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    let is_v = matches!(&event.logical_key, Key::Character(text) if text.eq_ignore_ascii_case("v"))
        || event.physical_key == PhysicalKey::Code(KeyCode::KeyV);
    command && is_v && event.state.is_pressed() && !event.repeat
}

/// Cmd+D duplicates the selected objects, Cmd+C copies them and Cmd+V pastes.
/// egui-winit turns Cmd+C into `Event::Copy`, and swallows Cmd+V when the
/// system clipboard holds no text, so the paste key comes from
/// [`EditorShell::handle_window_event`] as `paste_key`.
fn object_shortcuts(ctx: &egui::Context, state: &mut EditorState, paste_key: bool) {
    if state.play.is_playing() || ctx.text_edit_focused() || terminal_has_keyboard(ctx, state) {
        return;
    }
    let (duplicate, copy) = ctx.input(|input| {
        (
            input.modifiers.command && input.key_pressed(egui::Key::D),
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy)),
        )
    });
    if duplicate {
        MenuCommand::DuplicateSelection.apply(state);
    }
    if copy {
        MenuCommand::CopySelection.apply(state);
    }
    if paste_key {
        MenuCommand::Paste.apply(state);
    }
}

/// Whether the integrated terminal has the keyboard.
fn terminal_has_keyboard(ctx: &egui::Context, state: &EditorState) -> bool {
    #[cfg(feature = "terminal")]
    {
        state
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.has_keyboard(ctx))
    }
    #[cfg(not(feature = "terminal"))]
    {
        let _ = (ctx, state);
        false
    }
}

/// With Cmd (Ctrl off macOS), the key that saves.
const SAVE_KEY: egui::Key = egui::Key::S;

/// What Cmd+S does when egui has the keyboard: saves the scene if it has
/// unsaved changes, and the active code file while the Code panel is
/// open. (With Monaco focused, Monaco takes Cmd+S itself and saves only
/// its file; egui never sees the key.) The scene isn't saved in Play: the
/// world is then the running game, not the scene being built.
fn save_shortcut(state: &mut EditorState) {
    if state.panels.code {
        state.code_save_requested = true;
    }
    if state.play.is_playing() {
        return;
    }
    state.sync_scene_dirty();
    if state.dirty.scene() {
        MenuCommand::SaveScene.apply(state);
    }
}

/// Runs `action` now if nothing is unsaved; otherwise stashes it in
/// `state.pending_action` so [`unsaved_changes_modal`] can prompt.
pub(crate) fn request_guarded(state: &mut EditorState, action: PendingAction) {
    if state.has_unsaved_changes() {
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
        PendingAction::NewProject(path, language, template) => {
            state.project_request = Some(ProjectRequest::create_with_template(
                path, language, template,
            ))
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
                .add(egui::Button::new("Save").fill(crate::theme::NIGHT.accent_fill))
                .clicked()
                && save_before_pending(state)
            {
                choice = Some(Choice::Proceed);
            }
            if ui.button("Discard").clicked() {
                state.discard_unsaved_scene();
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
        if ui
            .add_enabled(
                state.selected_texture_asset().is_some(),
                egui::Button::new("Create Sprite from Selected Image"),
            )
            .clicked()
        {
            MenuCommand::CreateSprite.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(
                state.selected_texture_asset().is_some(),
                egui::Button::new("Create Tilemap from Selected Image"),
            )
            .clicked()
        {
            MenuCommand::CreateTilemap.apply(state);
            ui.close();
        }
        if ui.button("Create Water").clicked() {
            MenuCommand::CreateWater.apply(state);
            ui.close();
        }
        if ui.button("Create Point Light").clicked() {
            MenuCommand::CreatePointLight.apply(state);
            ui.close();
        }
        if ui.button("Create UI Label").clicked() {
            MenuCommand::CreateUiLabel.apply(state);
            ui.close();
        }
        if ui.button("Create UI Button").clicked() {
            MenuCommand::CreateUiButton.apply(state);
            ui.close();
        }
        if ui.button("Create Camera").clicked() {
            MenuCommand::CreateCamera.apply(state);
            ui.close();
        }
        if ui.button("Create Sound").clicked() {
            MenuCommand::CreateSound.apply(state);
            ui.close();
        }
        let has_selection = state.selection_len() > 0;
        if ui
            .add_enabled(has_selection, egui::Button::new("Duplicate   ⌘D"))
            .clicked()
        {
            MenuCommand::DuplicateSelection.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy Objects   ⌘C"))
            .clicked()
        {
            MenuCommand::CopySelection.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(
                state.can_paste_objects(),
                egui::Button::new("Paste Objects   ⌘V"),
            )
            .clicked()
        {
            MenuCommand::Paste.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Group Selection   ⌘G"))
            .clicked()
        {
            MenuCommand::GroupSelection.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Ungroup   ⇧⌘G"))
            .clicked()
        {
            MenuCommand::Ungroup.apply(state);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Delete Selected   ⌫"))
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
            state.overlays.all_colliders,
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
    Pause,
    Step,
    Close,
    Layers,
    Code,
    Check,
    Inspector,
    Profiler,
    Viewport,
    Pointer,
    Hand,
}

fn paint_brand_mark(ui: &mut egui::Ui) {
    let id = egui::Id::new("studio_app_logo");
    let texture = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(id))
        .or_else(|| {
            let bytes = include_bytes!("../../../editor/assets/app_logo.png");
            let image = match engine_renderer::decode_rgba8(bytes) {
                Ok(image) => image,
                Err(error) => {
                    tracing::error!(%error, "could not decode embedded studio logo");
                    return None;
                }
            };
            let pixels = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            let texture =
                ui.ctx()
                    .load_texture("studio_app_logo", pixels, egui::TextureOptions::LINEAR);
            ui.ctx()
                .data_mut(|data| data.insert_temp(id, texture.clone()));
            Some(texture)
        });
    if let Some(texture) = texture {
        ui.add(egui::Image::new((texture.id(), egui::vec2(23.0, 23.0))));
    } else {
        ui.add_space(23.0);
    }
}

impl ShellIcon {
    #[cfg(test)]
    const ALL: [Self; 13] = [
        Self::Run,
        Self::Stop,
        Self::Pause,
        Self::Step,
        Self::Close,
        Self::Layers,
        Self::Code,
        Self::Check,
        Self::Inspector,
        Self::Profiler,
        Self::Viewport,
        Self::Pointer,
        Self::Hand,
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
        ShellIcon::Pause => {
            for x in [4.5, 9.5] {
                painter.rect_filled(
                    egui::Rect::from_min_size(inset(x, 3.5), egui::vec2(2.5, 9.0)),
                    1.0,
                    color,
                );
            }
        }
        ShellIcon::Step => {
            painter.add(egui::Shape::convex_polygon(
                vec![inset(3.5, 3.5), inset(3.5, 12.5), inset(10.0, 8.0)],
                color,
                egui::Stroke::NONE,
            ));
            painter.rect_filled(
                egui::Rect::from_min_size(inset(10.5, 3.5), egui::vec2(2.5, 9.0)),
                1.0,
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
        ShellIcon::Pointer => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    inset(4.0, 2.0),
                    inset(4.0, 13.0),
                    inset(7.0, 10.5),
                    inset(9.5, 14.5),
                    inset(11.0, 13.5),
                    inset(8.5, 9.5),
                    inset(12.5, 9.0),
                ],
                egui::Color32::TRANSPARENT,
                stroke,
            ));
        }
        ShellIcon::Hand => {
            // Four fingers and a palm.
            for (x, top) in [(4.5, 4.0), (7.0, 2.5), (9.5, 3.0), (12.0, 5.0)] {
                painter.line_segment([inset(x, top), inset(x, 9.0)], stroke);
            }
            painter.add(egui::Shape::line(
                vec![
                    inset(4.5, 9.0),
                    inset(4.5, 11.0),
                    inset(6.5, 14.0),
                    inset(10.5, 14.0),
                    inset(12.0, 12.0),
                    inset(12.0, 9.0),
                ],
                stroke,
            ));
            painter.line_segment([inset(2.0, 8.0), inset(4.5, 10.5)], stroke);
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

fn view_tool_icon(tool: ViewTool) -> ShellIcon {
    match tool {
        ViewTool::Select => ShellIcon::Pointer,
        ViewTool::Hand => ShellIcon::Hand,
    }
}

/// A square toolbar button showing `icon`, highlighted when `selected`.
fn toolbar_icon_button(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    icon: ShellIcon,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let response = ui
        .add_sized(
            [30.0, 24.0],
            egui::Button::new("")
                .fill(if selected {
                    palette.selected
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::NONE)
                .corner_radius(7),
        )
        .on_hover_text(tooltip);
    paint_shell_icon_at_color(
        ui,
        egui::Rect::from_center_size(response.rect.center(), egui::vec2(16.0, 16.0)),
        icon,
        if selected || response.hovered() {
            palette.text
        } else {
            palette.muted
        },
    );
    response
}

pub(crate) fn panel_header(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    panel_header_in(ui, &crate::theme::NIGHT, contents);
}

/// A panel's title row with a hairline rule under it, in `palette`.
fn panel_header_in(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    contents: impl FnOnce(&mut egui::Ui),
) {
    let width = ui.available_width();
    let header = ui.horizontal(|ui| {
        ui.set_min_size(egui::vec2(width, PANEL_HEADER_HEIGHT));
        ui.add_space(11.0);
        contents(ui);
    });
    let rect = header.response.rect;
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0, palette.rule),
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
            .fill(crate::theme::NIGHT.hover)
            .stroke(egui::Stroke::NONE)
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
        crate::theme::NIGHT.accent
    } else {
        crate::theme::NIGHT.muted
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
            egui::Stroke::new(2.0, crate::theme::NIGHT.accent),
        );
    }
    response
}

/// Play and mode controls share the prototype's single top bar.
fn toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // A segmented track; the bar is the darkest surface, so the track
        // is one step lighter.
        egui::Frame::NONE
            .fill(crate::theme::NIGHT.panel)
            .corner_radius(9)
            .inner_margin(egui::Margin::same(2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for workspace in Workspace::PRIMARY {
                        let selected = state.workspace == workspace;
                        let fill = if selected {
                            crate::theme::NIGHT.selected
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
                crate::play::EditorMode::Building if state.play.is_compiling() => {
                    (crate::theme::NIGHT.accent, "BUILDING")
                }
                crate::play::EditorMode::Building => (crate::theme::NIGHT.accent, "STARTING"),
                crate::play::EditorMode::Play => (crate::theme::NIGHT.highlight, "PLAYING"),
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
            .circle_filled(dot.center(), 2.5, crate::theme::NIGHT.success);
        ui.label(format!(
            "Ln {}, Col {}",
            state.cursor_line, state.cursor_column
        ));
        ui.colored_label(crate::theme::NIGHT.subtle, "·");
        ui.label("Spaces: 4");
        ui.colored_label(crate::theme::NIGHT.subtle, "·");
        ui.label("UTF-8");
        ui.colored_label(crate::theme::NIGHT.subtle, "·");
        ui.label("Rust");
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match state.dirty.summary() {
                Some(summary) => {
                    ui.colored_label(
                        crate::theme::NIGHT.highlight,
                        format!("● Unsaved: {summary}"),
                    );
                }
                None => {
                    ui.colored_label(crate::theme::NIGHT.subtle, "Saved");
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
    lock_while_previewing(ui, state);
    panel_title(ui, ShellIcon::Inspector, "Inspector");
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(12, 13))
        .show(ui, |ui| {
            // A resizable panel keeps the width its content takes, and a
            // vertical scroll area grows sideways to fit its widest row. So
            // the content gets a fixed rect that the panel never sees past:
            // a row too wide is clipped instead of widening the Inspector.
            let rect = ui.available_rect_before_wrap();
            let mut content = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            content.set_clip_rect(rect.intersect(ui.clip_rect()));
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(&mut content, |ui| {
                    inspector::show(
                        ui,
                        &mut state.world,
                        state.selected_entity,
                        &state.secondary_selection,
                        &mut state.batch_edit,
                        &state.asset_index,
                        &mut state.open_script_request,
                    );
                });
            ui.advance_cursor_after_rect(rect);
        });
}

fn panel_title(ui: &mut egui::Ui, icon: ShellIcon, title: &str) {
    panel_header(ui, |ui| {
        paint_shell_icon(ui, icon);
        ui.label(crate::theme::semibold(title));
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
    let parent = ui.available_rect_before_wrap();
    let viewport_panel = egui::Panel::right("code_viewport_panel")
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
    paint_resize_grip(ui, parent, viewport_panel.response.rect);

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
        state.pending_action.is_some() || state.sdk_upgrade_message.is_some(),
        ui.ctx().any_popup_open(),
    );
    let pointer_down = ui.input(|input| input.pointer.any_down());
    state.code_panel_resizing = code_panel_is_resizing(
        state.prev_code_panel_rect,
        state.code_panel_rect,
        pointer_down,
    );
    state.prev_code_panel_rect = state.code_panel_rect;
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

/// The Scene viewport header exposes only controls backed by the editor:
/// the title, the Play controls in the middle, the view tools on the right.
fn scene_view_toolbar(ui: &mut egui::Ui, state: &mut EditorState) {
    let palette = &crate::theme::NIGHT;
    panel_header_in(ui, palette, |ui| {
        let header = ui.max_rect();
        let (icon, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        paint_shell_icon_at_color(ui, icon, ShellIcon::Viewport, palette.muted);
        ui.label(crate::theme::semibold("Viewport").color(palette.text));
        ui.label(egui::RichText::new("/ World").color(palette.subtle));
        let centre = egui::Rect::from_center_size(header.center(), egui::vec2(112.0, 30.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(centre)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| play_controls(ui, state),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            if !state.play.is_previewing() {
                view_tool_switch(ui, state);
            }
        });
    });
}

/// Play / Stop, Pause and Step for a preview inside the Scene View, in one
/// rounded group like the view tools.
fn play_controls(ui: &mut egui::Ui, state: &mut EditorState) {
    let palette = &crate::theme::NIGHT;
    crate::widgets::segmented(palette).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let previewing = state.play.is_previewing();
        let running_elsewhere = state.play.is_playing() && !previewing;
        let (icon, tooltip) = if previewing {
            (ShellIcon::Stop, "Stop (⌘P)")
        } else {
            (ShellIcon::Run, "Play in the Scene View (⌘P)")
        };
        let play = ui.add_enabled_ui(!running_elsewhere, |ui| {
            toolbar_icon_button(ui, palette, icon, tooltip, previewing)
        });
        if play.inner.clicked() {
            MenuCommand::TogglePreview.apply(state);
        }
        let live = previewing && state.play.mode() == crate::play::EditorMode::Play;
        let paused = state.play.is_paused();
        let pause = ui.add_enabled_ui(live, |ui| {
            toolbar_icon_button(
                ui,
                palette,
                ShellIcon::Pause,
                if paused {
                    "Resume (⇧⌘P)"
                } else {
                    "Pause (⇧⌘P)"
                },
                paused,
            )
        });
        if pause.inner.clicked() {
            MenuCommand::TogglePause.apply(state);
        }
        let step = ui.add_enabled_ui(live && paused, |ui| {
            toolbar_icon_button(ui, palette, ShellIcon::Step, "Step one frame (⌥⌘P)", false)
        });
        if step.inner.clicked() {
            MenuCommand::StepFrame.apply(state);
        }
    });
}

/// Select and Hand as one rounded segmented control: a soft pill holding
/// both buttons, with the active tool raised inside it.
fn view_tool_switch(ui: &mut egui::Ui, state: &mut EditorState) {
    let palette = &crate::theme::NIGHT;
    let active = state.active_view_tool();
    crate::widgets::segmented(palette).show(ui, |ui| {
        // The header lays out right to left, so add the tools in reverse:
        // Select still reads first, as in Figma.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for tool in ViewTool::ALL.into_iter().rev() {
                if toolbar_icon_button(
                    ui,
                    palette,
                    view_tool_icon(tool),
                    tool.tooltip(),
                    active == tool,
                )
                .clicked()
                {
                    state.view_tool = tool;
                }
            }
        });
    });
}

fn scene_view_footer(ui: &mut egui::Ui, state: &mut EditorState) {
    let palette = &crate::theme::NIGHT;
    let rect = ui.max_rect();
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0, palette.rule),
    );
    ui.horizontal(|ui| {
        crate::widgets::segmented(palette).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for dimension in crate::chrome::EditorDimension::ALL {
                if crate::widgets::segment_label(
                    ui,
                    palette,
                    state.dimension == dimension,
                    dimension.label(),
                )
                .clicked()
                {
                    state.dimension = dimension;
                    if !dimension.is_2d() {
                        state.tile_paint_active = false;
                    }
                }
            }
        });
        crate::widgets::segmented(palette).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for space in TransformSpace::ALL {
                if crate::widgets::segment_label(
                    ui,
                    palette,
                    state.transform_space == space,
                    space.label(),
                )
                .clicked()
                {
                    state.transform_space = space;
                }
            }
        });
        ui.add_space(4.0);
        // Menu titles read as quiet text until hovered, like a macOS toolbar.
        ui.visuals_mut().widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.visuals_mut().widgets.inactive.fg_stroke.color = palette.muted;
        ui.menu_button("Overlays", |ui| {
            ui.checkbox(&mut state.overlays.all_colliders, "All colliders")
                .on_hover_text("The selected object's colliders are always shown");
            ui.checkbox(&mut state.overlays.lights, "Lights");
            ui.checkbox(&mut state.overlays.cameras, "Cameras");
            ui.checkbox(&mut state.overlays.grid, "Ground grid");
            ui.checkbox(&mut state.overlays.audio, "Audio emitters");
            ui.checkbox(&mut state.overlays.nav, "Nav grid");
        });
        ui.menu_button("Terrain", |ui| {
            if ui.button("Create terrain").clicked() {
                state.create_terrain_request = true;
                ui.close();
            }
            if ui.button("Create water").clicked() {
                MenuCommand::CreateWater.apply(state);
                ui.close();
            }
            if ui
                .checkbox(&mut state.terrain_sculpt_active, "Sculpt in Scene View")
                .changed()
            {
                state.terrain_stroke = None;
                if state.terrain_sculpt_active {
                    state.tile_paint_active = false;
                }
            }
            crate::terrain_tools::show_controls(ui, &mut state.terrain_tools);
            let has_model = state.selected_asset.as_deref().is_some_and(|path| {
                crate::assets::AssetKind::from_path(path) == crate::assets::AssetKind::Mesh
            });
            if ui
                .add_enabled(
                    has_model && state.selected_entity.is_some(),
                    egui::Button::new("Scatter selected model on terrain"),
                )
                .on_hover_text("Select a terrain in Hierarchy and a model in Project Assets")
                .clicked()
            {
                if let Err(error) = state.scatter_selected_mesh_on_terrain() {
                    state.asset_browser.operation_error = Some(error);
                }
                ui.close();
            }
        });
        ui.menu_button("Tilemap", |ui| {
            if ui
                .add_enabled(
                    state.selected_texture_asset().is_some(),
                    egui::Button::new("Create from selected image"),
                )
                .clicked()
            {
                MenuCommand::CreateTilemap.apply(state);
                ui.close();
            }
            let selected_map = state.selected_entity.is_some_and(|entity| {
                state
                    .world
                    .get::<engine_scene::SceneTilemap>(entity)
                    .is_some()
            });
            ui.add_enabled_ui(selected_map && state.dimension.is_2d(), |ui| {
                if ui
                    .checkbox(&mut state.tile_paint_active, "Paint in Scene View")
                    .changed()
                    && state.tile_paint_active
                {
                    state.terrain_sculpt_active = false;
                    state.terrain_stroke = None;
                }
                ui.checkbox(&mut state.tile_erase_active, "Erase cells");
            });
            ui.weak("Select a tilemap, then click or drag in 2D Scene View.");
        });
        // The menu's contents run only while it is open: that is when the
        // viewport shows which way the sun shines.
        state.aiming_sun = false;
        ui.menu_button("Environment", |ui| {
            state.aiming_sun = true;
            environment_ambient_ui(ui, state);
            ui.separator();
            environment_start_state_ui(ui, state);
            ui.separator();
            ui.label("Lighting");
            if ui.button("Create point light").clicked() {
                MenuCommand::CreatePointLight.apply(state);
                ui.close();
            }
            ui.weak(format!(
                "Up to {} point lights nearest the camera light the scene",
                engine_renderer::MAX_POINT_LIGHTS
            ));
            let current = state
                .world
                .get_resource::<engine_scene::SceneLighting>()
                .copied();
            let mut enabled = current.is_some();
            if ui
                .checkbox(&mut enabled, "Override scene lighting")
                .changed()
            {
                if enabled {
                    state.world.insert_resource(engine_scene::SceneLighting(
                        engine_scene::LightingData::default(),
                    ));
                } else {
                    state.world.remove_resource::<engine_scene::SceneLighting>();
                }
                state.mark_dirty();
            }
            if enabled {
                let mut data = state
                    .world
                    .get_resource::<engine_scene::SceneLighting>()
                    .map(|value| value.0)
                    .unwrap_or_default();
                let mut changed = false;
                ui.label("Sun direction");
                for (axis, component) in ["X", "Y", "Z"].into_iter().zip(&mut data.sun_direction) {
                    changed |= ui
                        .add(egui::Slider::new(component, -1.0..=1.0).text(axis))
                        .changed();
                }
                changed |= ui
                    .add(
                        egui::Slider::new(&mut data.sun_intensity, 0.0..=20.0).text("Sun strength"),
                    )
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut data.ambient_intensity, 0.0..=5.0).text("Ambient"))
                    .changed();
                ui.label("Sun color");
                changed |= ui.color_edit_button_rgb(&mut data.sun_color).changed();
                if data
                    .sun_direction
                    .iter()
                    .map(|value| value * value)
                    .sum::<f32>()
                    < 0.000001
                {
                    ui.label("Sun direction cannot be zero");
                } else if changed {
                    state
                        .world
                        .insert_resource(engine_scene::SceneLighting(data));
                    state.mark_dirty();
                }
            }
        });
    });
}

/// Writes the scene's start state. "Playing" is the default, so it is
/// stored as no setting at all.
fn set_start_state(state: &mut EditorState, chosen: engine_scene::GameStateName) {
    let mut environment = state.environment();
    let value = (chosen != engine_scene::GameStateName::Playing).then_some(chosen);
    if environment.start_state == value {
        return;
    }
    environment.start_state = value;
    state.set_environment(environment);
}

/// The state the scene starts in, such as Menu for a title screen.
fn environment_start_state_ui(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.label("Start state");
    ui.weak("The game state when this scene begins.");
    let current = state.environment().start_state.unwrap_or_default();
    for candidate in engine_scene::GameStateName::ALL {
        let label = if candidate == engine_scene::GameStateName::Playing {
            "Playing (default)"
        } else {
            candidate.label()
        };
        if ui.radio(candidate == current, label).clicked() {
            set_start_state(state, candidate);
        }
    }
}

/// Scene-wide ambient sounds (wind, music): they loop at the same volume
/// everywhere, so they are settings rather than objects placed in the map.
fn environment_ambient_ui(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.label("Ambient sounds");
    ui.weak("Loop everywhere in the scene, like wind or music.");
    let mut environment = state.environment();
    let mut changed = false;
    let mut remove = None;
    for (index, ambient) in environment.ambient_sounds.iter_mut().enumerate() {
        let name = uuid::Uuid::parse_str(&ambient.sound.id)
            .ok()
            .and_then(|id| {
                state
                    .asset_index
                    .path_for(engine_asset::AssetId::from_uuid(id))
            })
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Missing sound".to_owned());
        ui.horizontal(|ui| {
            ui.label(name);
            changed |= ui
                .add(egui::Slider::new(&mut ambient.gain, 0.0..=4.0).text("Volume"))
                .changed();
            if ui.small_button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        environment.ambient_sounds.remove(index);
        changed = true;
    }
    if changed {
        state.set_environment(environment);
    }
    if ui
        .add_enabled(
            state.selected_audio_asset().is_some(),
            egui::Button::new("Add selected sound"),
        )
        .on_disabled_hover_text("Select a WAV sound in Project Assets first")
        .clicked()
        && let Err(error) = state.add_ambient_sound_from_selection()
    {
        state.asset_browser.operation_error = Some(error);
    }
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
            dock::Tab::Hierarchy => show_hierarchy(ui, self.state),
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
                handle_scene_view_input(ui, self.viewport, self.state, &response);
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
                    self.state.pending_action.is_some() || self.state.sdk_upgrade_message.is_some(),
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
            StudioTab::Hierarchy => show_hierarchy(ui, self.state),
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
                handle_scene_interaction(self.viewport, self.state, &response);
                handle_asset_drop(ui, self.viewport, self.state, &response);
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
        CameraGesture, DragButtons, ScrollIntent, ShellIcon, ShellLayout, ViewTool, camera_gesture,
        code_panel_is_resizing, code_webview_visible, paint_shell_icon, scene_cursor_icon,
        scroll_intent, shell_layout, ui_preview_rect, viewport_render_size,
    };
    use crate::chrome::Workspace;

    #[test]
    fn ui_preview_uses_the_same_screen_anchor_as_play() {
        let screen = egui::Rect::from_min_size(egui::pos2(50.0, 100.0), egui::vec2(400.0, 200.0));
        let mut data = engine_scene::UiElementData::button();
        assert_eq!(
            ui_preview_rect(screen, &data),
            egui::Rect::from_min_size(egui::pos2(160.0, 178.0), egui::vec2(180.0, 44.0))
        );
        data.anchor = engine_scene::UiAnchor::TopRight;
        data.offset = [-20.0, 10.0];
        assert_eq!(
            ui_preview_rect(screen, &data),
            egui::Rect::from_min_size(egui::pos2(250.0, 110.0), egui::vec2(180.0, 44.0))
        );
    }

    #[test]
    fn new_project_dialog_recommends_lua() {
        assert_eq!(
            super::NewProjectDialog::default().language,
            engine_project::GameplayLanguage::Lua
        );
    }

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
        // A random name: tests run in parallel, and two can start within
        // the clock's resolution.
        let root = std::env::temp_dir().join(format!(
            "vge_menu_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
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
    fn start_state_menu_writes_none_for_playing() {
        use engine_scene::GameStateName;
        let (root, mut state) = menu_test_state();
        super::set_start_state(&mut state, GameStateName::Paused);
        assert_eq!(state.environment().start_state, Some(GameStateName::Paused));
        super::set_start_state(&mut state, GameStateName::Menu);
        assert_eq!(state.environment().start_state, Some(GameStateName::Menu));
        super::set_start_state(&mut state, GameStateName::Playing);
        assert_eq!(state.environment().start_state, None);
        assert!(
            state
                .world
                .get_resource::<engine_scene::SceneEnvironment>()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_asset_preview_sits_beside_a_wide_tree_and_under_a_narrow_one() {
        let (root, mut state) = menu_test_state();
        state.assets.push(crate::assets::AssetEntry {
            relative_path: std::path::PathBuf::from("player.png"),
            kind: crate::assets::AssetKind::Texture,
            id: None,
        });
        state.selected_asset = Some(std::path::PathBuf::from("player.png"));
        for (width, beside) in [(640.0, true), (240.0, false)] {
            let ctx = egui::Context::default();
            let mut tree = None;
            let mut preview = None;
            for frame in 0..8 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 320.0),
                    )),
                    time: Some(f64::from(frame) * 0.1),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| super::show_asset_browser(ui, &mut state));
                if frame == 7 {
                    for shape in &output.shapes {
                        if let egui::epaint::Shape::Text(text) = &shape.shape {
                            let rect = text.visual_bounding_rect();
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
            if beside {
                assert!(preview.min.x > tree.max.x, "{width}: {tree:?} {preview:?}");
                assert!((preview.min.y - tree.min.y).abs() < 30.0);
            } else {
                assert!(preview.min.y > tree.max.y, "{width}: {tree:?} {preview:?}");
            }
        }
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
    fn save_shortcut_saves_the_scene_and_the_open_code_file() {
        let (root, mut state) = menu_test_state();
        state.world.spawn(engine_ecs::components::Name::new("Cube"));
        state.mark_dirty();
        state.panels.code = false;

        super::save_shortcut(&mut state);
        assert!(!state.dirty.scene(), "the scene is saved");
        assert!(state.scene_path.is_file());
        assert!(
            !state.code_save_requested,
            "no code file without the Code panel"
        );

        state.panels.code = true;
        super::save_shortcut(&mut state);
        assert!(state.code_save_requested, "the open code file is saved too");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Runs two egui frames with a text field — focused when `typing` —
    /// pressing `key` in the second, and applies the delete shortcut there.
    fn press_for_delete(state: &mut crate::EditorState, key: egui::Key, typing: bool) {
        let ctx = egui::Context::default();
        let mut text = String::new();
        // The first frame settles focus, the second carries the key.
        for events in [
            Vec::new(),
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ] {
            let pressing = !events.is_empty();
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let response = ui.text_edit_singleline(&mut text);
                if typing {
                    response.request_focus();
                }
                if pressing {
                    super::delete_shortcut(ui.ctx(), state);
                }
            });
            // Nothing is drawn, so the font texture upload is dropped.
            output.textures_delta.clear();
        }
    }

    #[test]
    fn backspace_or_delete_removes_the_selection_but_not_while_typing() {
        let (root, mut state) = menu_test_state();
        let cube = state
            .world
            .spawn(engine_ecs::components::Name::new("Cube"))
            .id();
        state.select_only(cube);

        press_for_delete(&mut state, egui::Key::Backspace, true);
        assert!(state.world.get_entity(cube).is_ok(), "typing keeps it");

        press_for_delete(&mut state, egui::Key::Backspace, false);
        assert!(state.world.get_entity(cube).is_err(), "Backspace deletes");
        assert!(state.dirty.scene());

        let sphere = state
            .world
            .spawn(engine_ecs::components::Name::new("Sphere"))
            .id();
        state.select_only(sphere);
        press_for_delete(&mut state, egui::Key::Delete, false);
        assert!(state.world.get_entity(sphere).is_err(), "Delete deletes");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn save_shortcut_leaves_the_scene_alone_in_play() {
        let (root, mut state) = menu_test_state();
        state.mark_dirty();
        state.play.start(&mut state.world);

        super::save_shortcut(&mut state);
        assert!(
            state.dirty.scene(),
            "the running game is not saved as the scene"
        );
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
    fn trackpad_scroll_pans_command_scroll_orbits_and_wheels_zoom() {
        let wheel = |unit, delta: egui::Vec2, modifiers| egui::Event::MouseWheel {
            unit,
            delta,
            phase: egui::TouchPhase::Move,
            modifiers,
        };
        let none = egui::Modifiers::NONE;
        let command = egui::Modifiers {
            command: true,
            ..none
        };
        let notch = scroll_intent(&[wheel(
            egui::MouseWheelUnit::Line,
            egui::vec2(0.0, 2.0),
            none,
        )]);
        assert_eq!(notch.zoom_steps, 2.0);
        assert_eq!(
            (notch.pan, notch.orbit),
            (glam::Vec2::ZERO, glam::Vec2::ZERO)
        );

        let trackpad = scroll_intent(&[wheel(
            egui::MouseWheelUnit::Point,
            egui::vec2(12.0, -30.0),
            none,
        )]);
        assert_eq!(trackpad.pan, glam::Vec2::new(12.0, -30.0));
        assert_eq!(
            (trackpad.zoom_steps, trackpad.orbit),
            (0.0, glam::Vec2::ZERO)
        );

        let rotate = scroll_intent(&[wheel(
            egui::MouseWheelUnit::Point,
            egui::vec2(8.0, 4.0),
            command,
        )]);
        assert_eq!(
            rotate.orbit,
            glam::Vec2::new(8.0, 4.0),
            "⌘ + two-finger scroll rotates"
        );
        assert_eq!((rotate.zoom_steps, rotate.pan), (0.0, glam::Vec2::ZERO));

        let pinch = scroll_intent(&[egui::Event::Zoom(1.0 / 0.88)]);
        assert!((pinch.zoom_steps - 2.5).abs() < 1e-4, "pinch is amplified");
        assert_eq!(
            scroll_intent(&[egui::Event::Zoom(f32::NAN)]),
            ScrollIntent::default()
        );
    }

    #[test]
    fn camera_gestures_follow_editor_mouse_conventions() {
        let none = egui::Modifiers::NONE;
        let alt = egui::Modifiers { alt: true, ..none };
        let shift = egui::Modifiers {
            shift: true,
            ..none
        };
        let right = DragButtons {
            secondary: true,
            ..DragButtons::default()
        };
        let middle = DragButtons {
            middle: true,
            ..DragButtons::default()
        };
        let left = DragButtons {
            primary: true,
            ..DragButtons::default()
        };
        assert_eq!(
            camera_gesture(right, none, false, false, ViewTool::Select),
            Some(CameraGesture::Orbit)
        );
        assert_eq!(
            camera_gesture(right, none, false, true, ViewTool::Select),
            Some(CameraGesture::Look)
        );
        assert_eq!(
            camera_gesture(right, shift, false, false, ViewTool::Select),
            Some(CameraGesture::Pan)
        );
        assert_eq!(
            camera_gesture(middle, none, false, false, ViewTool::Select),
            Some(CameraGesture::Pan)
        );
        assert_eq!(
            camera_gesture(left, alt, false, false, ViewTool::Select),
            None,
            "⌥ + drag is not a camera gesture"
        );
        let command = egui::Modifiers {
            command: true,
            ..none
        };
        assert_eq!(
            camera_gesture(left, command, false, false, ViewTool::Select),
            Some(CameraGesture::Orbit),
            "⌘ + drag rotates on a trackpad"
        );
        assert_eq!(
            camera_gesture(left, command, true, false, ViewTool::Select),
            Some(CameraGesture::Pan)
        );
        assert_eq!(
            camera_gesture(left, none, false, false, ViewTool::Select),
            None,
            "left-drag stays with the gizmo"
        );
        assert_eq!(
            camera_gesture(right, none, true, false, ViewTool::Select),
            Some(CameraGesture::Pan),
            "2D only pans"
        );
        assert_eq!(
            camera_gesture(DragButtons::default(), alt, false, false, ViewTool::Select),
            None
        );

        // The Hand tool turns a plain left-drag into a pan; ⌘ + drag still rotates.
        assert_eq!(
            camera_gesture(left, none, false, false, ViewTool::Hand),
            Some(CameraGesture::Pan)
        );
        assert_eq!(
            camera_gesture(left, command, false, false, ViewTool::Hand),
            Some(CameraGesture::Orbit)
        );
        assert_eq!(
            camera_gesture(DragButtons::default(), none, false, false, ViewTool::Hand),
            None,
            "no drag, no gesture"
        );
    }

    /// A tree inside a group that is moved, turned a quarter around Y and
    /// scaled 2×, like the Island's "Tree 001" groups.
    fn tree_in_a_group() -> (
        engine_ecs::prelude::World,
        engine_ecs::prelude::Entity,
        engine_ecs::prelude::Entity,
    ) {
        use engine_ecs::components::{Name, Transform};
        use engine_ecs::prelude::ChildOf;
        let mut world = engine_ecs::prelude::World::new();
        let group = world
            .spawn((
                Name::new("Tree 001"),
                Transform::from(engine_utils::Transform {
                    translation: glam::Vec3::new(10.0, 0.0, 5.0),
                    rotation: glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                    scale: glam::Vec3::splat(2.0),
                }),
            ))
            .id();
        let trunk = world
            .spawn((
                Name::new("Trunk"),
                Transform::from(engine_utils::Transform::from_translation(glam::Vec3::new(
                    1.0, 0.0, 0.0,
                ))),
                ChildOf(group),
            ))
            .id();
        (world, group, trunk)
    }

    #[test]
    fn the_gizmo_sits_on_an_object_inside_a_group() {
        let (_root, mut state) = menu_test_state();
        let (world, _group, trunk) = tree_in_a_group();
        state.world = world;
        state.select_only(trunk);
        let origin = state.gizmo_origin().expect("gizmo");
        // Local x 1, scaled 2, turned to -Z, from (10, 0, 5).
        assert!(
            (origin - glam::Vec3::new(10.0, 0.0, 3.0)).length() < 1e-4,
            "{origin}"
        );
    }

    #[test]
    fn gizmo_drags_follow_world_axes_inside_a_turned_scaled_group() {
        use crate::gizmo::Axis;
        let (mut world, group, trunk) = tree_in_a_group();
        let before = crate::hierarchy::world_transform(&world, trunk);
        super::apply_gizmo_edit(&mut world, &[trunk], super::GizmoEdit::Move(Axis::X, 3.0));
        let after = crate::hierarchy::world_transform(&world, trunk);
        assert!(
            (after.translation - before.translation - glam::Vec3::new(3.0, 0.0, 0.0)).length()
                < 1e-4,
            "moved exactly 3 along world X: {before:?} → {after:?}"
        );
        super::apply_gizmo_edit(&mut world, &[trunk], super::GizmoEdit::Rotate(Axis::Y, 0.5));
        let turned = crate::hierarchy::world_transform(&world, trunk);
        assert!(
            (turned.translation - after.translation).length() < 1e-4,
            "turning in place does not move it"
        );

        // Group and trunk both selected: the trunk moves once, with the group.
        let trunk_before = crate::hierarchy::world_transform(&world, trunk).translation;
        super::apply_gizmo_edit(
            &mut world,
            &[group, trunk],
            super::GizmoEdit::Move(Axis::Y, 1.0),
        );
        let trunk_after = crate::hierarchy::world_transform(&world, trunk).translation;
        assert!(
            (trunk_after - trunk_before - glam::Vec3::Y).length() < 1e-4,
            "{trunk_before} → {trunk_after}"
        );
    }

    /// Writes `studio.png`: the whole Studio window, built from the same
    /// panel functions as the real one (the 3D view is a placeholder), for a
    /// visual check of the shared style. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_studio_window() {
        use engine_ecs::components::{Name, Transform};
        use engine_ecs::prelude::ChildOf;
        let (root, mut state) = menu_test_state();
        let at = |x: f32| {
            Transform::from(engine_utils::Transform::from_translation(glam::Vec3::new(
                x, 0.0, 0.0,
            )))
        };
        state.world.spawn((Name::new("Terrain"), at(0.0)));
        let trees = state.world.spawn((Name::new("Trees"), at(0.0))).id();
        for index in 1..=3 {
            let tree = state
                .world
                .spawn((
                    Name::new(format!("Tree {index:03}")),
                    at(index as f32),
                    ChildOf(trees),
                ))
                .id();
            state.world.spawn((
                Name::new(format!("Tree {index:03} Trunk")),
                at(0.0),
                ChildOf(tree),
            ));
        }
        let camera = state.world.spawn((Name::new("Main Camera"), at(5.0))).id();
        state.select_only(camera);
        for (path, kind) in [
            ("models/tree.gltf", crate::assets::AssetKind::Mesh),
            ("models/rock.gltf", crate::assets::AssetKind::Mesh),
            ("textures/grass.png", crate::assets::AssetKind::Texture),
            ("audio/wind.wav", crate::assets::AssetKind::Audio),
        ] {
            state.assets.push(crate::assets::AssetEntry {
                relative_path: std::path::PathBuf::from(path),
                kind,
                id: None,
            });
        }
        state.asset_browser.open_folders = [std::path::PathBuf::new(), "models".into()].into();
        state.hierarchy.open = ["unused".to_owned()].into();
        state.output.write_line("[play] building 'island' (Debug)");
        state
            .output
            .write_line("[build:stderr]    Compiling island v0.1.0");
        let mut new_project = None;
        let path = crate::ui_snapshot::snapshot_dir().join("studio.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(1280.0, 800.0), 2.0, |ui| {
            egui::Panel::top("snapshot_header")
                .exact_size(super::STUDIO_HEADER_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(crate::theme::NIGHT.shell)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ui, |ui| {
                    super::studio_header_contents(ui, &mut state, &mut new_project, true)
                });
            egui::Panel::bottom("snapshot_status")
                .exact_size(super::STUDIO_STATUS_HEIGHT)
                .show_separator_line(false)
                .frame(
                    egui::Frame::NONE
                        .fill(crate::theme::NIGHT.shell)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ui, |ui| super::status_bar(ui, &state));
            egui::Panel::left("snapshot_left")
                .exact_size(250.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| {
                    egui::Panel::bottom("snapshot_assets")
                        .exact_size(260.0)
                        .frame(super::studio_panel_frame())
                        .show_separator_line(false)
                        .show(ui, |ui| super::show_asset_browser(ui, &mut state));
                    super::show_hierarchy(ui, &mut state);
                });
            egui::Panel::right("snapshot_inspector")
                .exact_size(290.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
            egui::Panel::bottom("snapshot_console")
                .exact_size(190.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| super::bottom_tabs_panel(ui, &mut state));
            egui::CentralPanel::default()
                .frame(super::studio_panel_frame())
                .show(ui, |ui| {
                    super::scene_view_toolbar(ui, &mut state);
                    let rest = ui.available_rect_before_wrap();
                    ui.painter().rect_filled(
                        rest.shrink(1.0),
                        0.0,
                        egui::Color32::from_rgb(0x2B, 0x3A, 0x2E),
                    );
                });
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A tree trunk with the sections the Inspector shows most: transform,
    /// collider, visual rules and a material override.
    fn inspector_test_state() -> (std::path::PathBuf, crate::EditorState) {
        use engine_ecs::components::{MeshSource, Name, Static, Transform};
        let (root, mut state) = menu_test_state();
        let mut transform =
            engine_utils::Transform::from_translation(glam::Vec3::new(0.0, 0.681, 0.0));
        transform.rotation = glam::Quat::from_euler(
            glam::EulerRot::XYZ,
            4.75f32.to_radians(),
            -33.15f32.to_radians(),
            8.65f32.to_radians(),
        );
        transform.scale = glam::Vec3::splat(1.229);
        let trunk = state
            .world
            .spawn((
                Name::new("Tree 110 Trunk"),
                Transform::from(transform),
                Static,
                MeshSource::new("mesh-id", "material-id"),
                engine_scene::SceneCollider(engine_scene::ColliderData::new(
                    engine_scene::ColliderShape::Cylinder {
                        half_height: 1.6,
                        radius: 0.35,
                    },
                    engine_scene::BodyKind::Fixed,
                )),
                engine_scene::SceneRules(vec![engine_scene::RuleData {
                    trigger: engine_scene::RuleTrigger::Named("game_start".into()),
                    action: engine_scene::RuleAction::EmitEvent {
                        name: "open_gate".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: engine_scene::RuleMatch::All,
                    graph_position: None,
                }]),
                engine_scene::SceneMaterialOverride(engine_scene::MaterialOverrideData {
                    tint: [1.0, 1.0, 1.0],
                    metallic: None,
                    roughness: Some(0.6),
                }),
            ))
            .id();
        state.select_only(trunk);
        (root, state)
    }

    /// Writes `inspector.png` (a wide panel) and `inspector-narrow.png`, for
    /// a visual check. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_inspector_panel() {
        let (root, mut state) = inspector_test_state();
        for (name, width) in [("inspector.png", 420.0), ("inspector-narrow.png", 270.0)] {
            let path = crate::ui_snapshot::snapshot_dir().join(name);
            crate::ui_snapshot::render_png(&path, egui::vec2(width + 20.0, 1040.0), 2.0, |ui| {
                egui::Panel::right("snapshot_inspector")
                    .exact_size(width)
                    .frame(super::studio_panel_frame())
                    .show_separator_line(false)
                    .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(crate::theme::NIGHT.shell))
                    .show(ui, |_| {});
            })
            .expect("snapshot");
        }
        let _ = std::fs::remove_dir_all(root);

        // An object without a 3D model offers to take an image.
        let (root, mut state) = menu_test_state();
        let spawn = state
            .world
            .spawn((
                engine_ecs::components::Name::new("Spawn point"),
                engine_ecs::components::Transform::from(engine_utils::Transform::IDENTITY),
            ))
            .id();
        state.select_only(spawn);
        let path = crate::ui_snapshot::snapshot_dir().join("inspector-empty.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(440.0, 520.0), 2.0, |ui| {
            egui::Panel::right("snapshot_inspector")
                .exact_size(420.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(crate::theme::NIGHT.shell))
                .show(ui, |_| {});
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `inspector-collider.png`: a Dynamic sprite with a collider, so
    /// the physics rows and the fit buttons can be checked. Needs a GPU
    /// adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_inspector_collider() {
        let (root, mut state) = menu_test_state();
        let mut data = engine_scene::ColliderData::new(
            engine_scene::ColliderShape::Ball { radius: 0.5 },
            engine_scene::BodyKind::Dynamic,
        );
        data.mass = Some(2.5);
        let crate_entity = state
            .world
            .spawn((
                engine_ecs::components::Name::new("Crate"),
                engine_ecs::components::Transform::from(engine_utils::Transform::IDENTITY),
                engine_ecs::components::SpriteSource {
                    atlas: uuid::Uuid::nil().to_string(),
                    region: "full".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                engine_scene::SceneCollider(data),
            ))
            .id();
        state.select_only(crate_entity);
        let path = crate::ui_snapshot::snapshot_dir().join("inspector-collider.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(440.0, 900.0), 2.0, |ui| {
            egui::Panel::right("snapshot_inspector")
                .exact_size(420.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(crate::theme::NIGHT.shell))
                .show(ui, |_| {});
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `inspector-camera.png`: the Camera section of an active
    /// camera. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_inspector_camera() {
        let (root, mut state) = menu_test_state();
        let camera = state
            .world
            .spawn((
                engine_ecs::components::Name::new("Main Camera"),
                engine_ecs::components::Transform::from(engine_utils::Transform::from_translation(
                    glam::Vec3::new(0.0, 2.0, 6.0),
                )),
                engine_ecs::components::Camera::from(engine_renderer::Camera::new(
                    glam::Vec3::ZERO,
                    glam::Vec3::NEG_Z,
                    16.0 / 9.0,
                )),
                engine_ecs::components::ActiveCamera,
            ))
            .id();
        let player_id = "00000000-0000-4000-8000-000000000001".to_string();
        state.world.spawn((
            engine_ecs::components::Name::new("Player"),
            engine_ecs::components::Transform::from(engine_utils::Transform::IDENTITY),
            engine_scene::SceneObjectId(player_id.clone()),
        ));
        state
            .world
            .entity_mut(camera)
            .insert(engine_ecs::components::CameraFollow::new(
                player_id,
                engine_ecs::components::FollowMode::THIRD_PERSON,
            ));
        state.select_only(camera);
        for (file, mode) in [
            (
                "inspector-camera.png",
                engine_ecs::components::FollowMode::THIRD_PERSON,
            ),
            (
                "inspector-camera-follow-first-person.png",
                engine_ecs::components::FollowMode::FIRST_PERSON,
            ),
            (
                "inspector-camera-follow-2d.png",
                engine_ecs::components::FollowMode::Side2D {
                    dead_zone: [2.0, 1.0],
                    bounds: Some([-10.0, -10.0, 10.0, 10.0]),
                    smoothing: 8.0,
                },
            ),
        ] {
            if let Some(mut follow) = state
                .world
                .get_mut::<engine_ecs::components::CameraFollow>(camera)
            {
                follow.mode = mode;
                follow.switch =
                    matches!(mode, engine_ecs::components::FollowMode::FirstPerson { .. }).then(
                        || engine_ecs::components::ViewSwitch {
                            key: "V".to_owned(),
                            other: engine_ecs::components::FollowMode::THIRD_PERSON,
                        },
                    );
            }
            let path = crate::ui_snapshot::snapshot_dir().join(file);
            crate::ui_snapshot::render_png(&path, egui::vec2(440.0, 1100.0), 2.0, |ui| {
                egui::Panel::right("snapshot_inspector")
                    .exact_size(420.0)
                    .frame(super::studio_panel_frame())
                    .show_separator_line(false)
                    .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(crate::theme::NIGHT.shell))
                    .show(ui, |_| {});
            })
            .expect("snapshot");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `inspector-rules.png`: a Timer rule with two conditions on
    /// "Any" and a "Set game state" action. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_inspector_rules() {
        use engine_scene::{
            GameStateName, RuleAction, RuleCompare, RuleCondition, RuleData, RuleMatch, RuleTrigger,
        };
        let (root, mut state) = menu_test_state();
        let condition = |variable: &str, compare, value| RuleCondition {
            variable: variable.into(),
            compare,
            value,
        };
        let owner = state
            .world
            .spawn((
                engine_ecs::components::Name::new("Round timer"),
                engine_ecs::components::Transform::from(engine_utils::Transform::IDENTITY),
                engine_scene::SceneRules(vec![
                    RuleData {
                        trigger: RuleTrigger::Timer {
                            seconds: 30.0,
                            repeat: true,
                        },
                        action: RuleAction::SetGameState {
                            state: GameStateName::GameOver,
                        },
                        conditions: vec![
                            condition("lives", RuleCompare::LessOrEqual, 0.0),
                            condition("time_left", RuleCompare::LessOrEqual, 0.0),
                        ],
                        match_mode: RuleMatch::Any,
                        graph_position: None,
                    },
                    RuleData::new(
                        RuleTrigger::CollisionEnter { with: None },
                        RuleAction::ChangeScene {
                            path: "scenes/level2.ron".into(),
                        },
                    ),
                ]),
            ))
            .id();
        state.select_only(owner);
        let path = crate::ui_snapshot::snapshot_dir().join("inspector-rules.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(440.0, 1000.0), 2.0, |ui| {
            egui::Panel::right("snapshot_inspector")
                .exact_size(420.0)
                .frame(super::studio_panel_frame())
                .show_separator_line(false)
                .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(crate::theme::NIGHT.shell))
                .show(ui, |_| {});
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `console.png` (a wide Console) and `console-narrow.png`, with
    /// the lines Studio logs at start-up, for a visual check. Needs a GPU
    /// adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_console_panel() {
        use tracing_subscriber::layer::SubscriberExt;
        let (root, mut state) = menu_test_state();
        let subscriber = tracing_subscriber::registry()
            .with(crate::console::ConsoleLayer::new(state.console.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "editor", total = 21, imported = 21, cached = 0, failed = 0, "asset import pass complete");
            tracing::warn!(target: "editor", "recovered unsaved scene changes from a previous session; save to keep them");
            tracing::info!(target: "editor", "editor window ready");
            tracing::info!(target: "engine_renderer::gpu", adapter = "Apple M1 Pro", format = "Bgra8UnormSrgb", width = 3024, height = 1834, msaa = 4, "GPU context initialized");
            // As the `log` bridge reports a record: its real target in a
            // `log.target` field.
            tracing::warn!(
                log.target = "egui_wgpu::renderer",
                log.module_path = "egui_wgpu::renderer",
                log.file = "/Users/me/.cargo/registry/src/egui-wgpu-0.36.1/src/renderer.rs",
                log.line = 407,
                "Detected a linear (sRGBA aware) framebuffer Bgra8UnormSrgb. egui prefers Rgba8Unorm or Bgra8Unorm"
            );
            tracing::info!(target: "editor_code", monaco_dir = "/Applications/RustyEngine Studio.app/Contents/Resources/monaco", "code editor webview created");
            tracing::info!(target: "editor_lsp", root = "/Users/me/Projects/my-game", server = "rust-analyzer", "language server started");
            tracing::error!(target: "engine_editor::play", "build failed: see the Output tab");
            tracing::debug!(target: "engine_editor::assets", "watching 4 folders");
        });
        state.console_filter.min_level = crate::console::LevelFilter::Debug;
        for (name, width) in [("console.png", 860.0), ("console-narrow.png", 420.0)] {
            let path = crate::ui_snapshot::snapshot_dir().join(name);
            crate::ui_snapshot::render_png(&path, egui::vec2(width, 380.0), 2.0, |ui| {
                egui::CentralPanel::default()
                    .frame(super::studio_panel_frame())
                    .show(ui, |ui| super::bottom_tabs_panel(ui, &mut state));
            })
            .expect("snapshot");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Opening sections must never push the panel wider than the width the
    /// user gave it: a resizable panel keeps whatever width its content
    /// takes, so one wide row used to widen the Inspector for good.
    #[test]
    fn opening_inspector_sections_never_widens_the_panel() {
        let (root, mut state) = inspector_test_state();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut width = 0.0;
        for frame in 0..8 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 900.0),
                    )),
                    time: Some(f64::from(frame) * 0.5),
                    ..Default::default()
                },
                |ui| {
                    let panel = egui::Panel::right("width_inspector")
                        .resizable(true)
                        .default_size(240.0)
                        .frame(super::studio_panel_frame())
                        .show_separator_line(false)
                        .show(ui, |ui| super::inspector_tabs_panel(ui, &mut state));
                    width = panel.response.rect.width();
                },
            );
            output.textures_delta.clear();
        }
        let _ = std::fs::remove_dir_all(root);
        assert!(width <= 247.0, "the Inspector grew to {width} points");
    }

    /// Writes `panel-grips.png`: three resizable panels around a central
    /// one, showing the resize grips in the gaps, for a visual check. Needs
    /// a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_panel_grips() {
        use super::StudioPanelExt;
        let path = crate::ui_snapshot::snapshot_dir().join("panel-grips.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(640.0, 400.0), 2.0, |ui| {
            for (panel, size) in [
                (egui::Panel::left("grip_left"), 160.0),
                (egui::Panel::right("grip_right"), 160.0),
                (egui::Panel::bottom("grip_bottom"), 120.0),
            ] {
                panel
                    .resizable(true)
                    .default_size(size)
                    .frame(super::studio_panel_frame())
                    .show_separator_line(false)
                    .show_studio_collapsible(ui, &mut true, |ui| {
                        ui.allocate_space(ui.available_size());
                    });
            }
            egui::CentralPanel::default()
                .frame(super::studio_panel_frame())
                .show(ui, |ui| {
                    ui.allocate_space(ui.available_size());
                });
        })
        .expect("snapshot");
    }

    /// Writes `bottom-row.png`: the Scene workspace's bottom row (Project
    /// Assets beside the Console) under a stand-in viewport, for a visual
    /// check of the gaps and grips. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_scene_bottom_row() {
        let (root, mut state) = menu_test_state();
        state.panels.asset_browser = true;
        state.panels.console = true;
        let path = crate::ui_snapshot::snapshot_dir().join("bottom-row.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(900.0, 420.0), 2.0, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, crate::theme::NIGHT.shell);
            super::show_scene_bottom_row(ui, &mut state, 220.0, 320.0);
            egui::CentralPanel::default()
                .frame(super::studio_panel_frame())
                .show(ui, |ui| {
                    ui.allocate_space(ui.available_size());
                });
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes `drop-marker.png`: where a dragged file will land.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_drop_marker() {
        let path = crate::ui_snapshot::snapshot_dir().join("drop-marker.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(320.0, 120.0), 2.0, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, 0.0, crate::theme::NIGHT.panel);
            super::paint_drop_marker(ui, rect, egui::pos2(60.0, 60.0), "barrel.gltf");
        })
        .expect("snapshot");
    }

    /// Writes `scene-chrome.png`: the Scene panel's header and footer, for
    /// a visual check. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_scene_panel_chrome() {
        let (root, mut state) = menu_test_state();
        let path = crate::ui_snapshot::snapshot_dir().join("scene-chrome.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(900.0, 240.0), 2.0, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, crate::theme::NIGHT.shell);
            super::studio_panel_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                crate::theme::NIGHT.apply(ui.visuals_mut());
                super::scene_view_toolbar(ui, &mut state);
                ui.add_space(140.0);
                super::scene_view_footer(ui, &mut state);
            });
        })
        .expect("snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A drag on a small handle must be judged from where the button went
    /// down: egui reports the drag only after the pointer has moved, by
    /// which time it can be off a scale knob and on its move arrow.
    #[test]
    fn a_drag_starts_where_the_button_was_pressed() {
        let ctx = egui::Context::default();
        let press = egui::pos2(100.0, 100.0);
        // egui hit-tests against the previous frame's widgets, so the first
        // frame only lays the area out.
        let frames = [
            vec![egui::Event::PointerMoved(press)],
            vec![egui::Event::PointerButton {
                pos: press,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![egui::Event::PointerMoved(egui::pos2(104.0, 100.0))],
            vec![egui::Event::PointerMoved(egui::pos2(115.0, 100.0))],
        ];
        let mut start = None;
        for (frame, events) in frames.into_iter().enumerate() {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(300.0, 300.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let response = ui.allocate_rect(ui.max_rect(), egui::Sense::drag());
                    if response.drag_started() {
                        start = super::drag_start_point(&response);
                    }
                },
            );
            output.textures_delta.clear();
        }
        assert_eq!(start, Some(press));
    }

    #[test]
    fn the_scene_view_leaves_the_cursor_alone_when_the_pointer_is_elsewhere() {
        assert!(!super::scene_owns_cursor(false, false, false, false));
        assert!(super::scene_owns_cursor(true, false, false, false));
        assert!(
            super::scene_owns_cursor(false, true, false, false),
            "a drag that leaves the view keeps its cursor"
        );
        assert!(super::scene_owns_cursor(false, false, true, false));
        assert!(super::scene_owns_cursor(false, false, false, true));
    }

    #[test]
    fn scene_is_neutral_until_a_gizmo_drag_is_active() {
        use crate::gizmo::{Axis, GizmoHandle};
        assert_eq!(
            scene_cursor_icon(false, None, None),
            egui::CursorIcon::Default
        );
        assert_eq!(
            scene_cursor_icon(true, None, None),
            egui::CursorIcon::Default,
            "click-dragging the scene without an axis must not show a grab cursor"
        );
        assert_eq!(
            scene_cursor_icon(true, Some(GizmoHandle::Move(Axis::X)), None),
            egui::CursorIcon::Grabbing
        );
        assert_eq!(
            scene_cursor_icon(false, None, Some(GizmoHandle::Rotate(Axis::Y))),
            egui::CursorIcon::None,
            "a hovered ring swaps the system cursor for the drawn rotate cursor"
        );
        assert_eq!(
            scene_cursor_icon(true, Some(GizmoHandle::Rotate(Axis::Y)), None),
            egui::CursorIcon::None
        );
        for scale in [GizmoHandle::ScaleAxis(Axis::X), GizmoHandle::Scale] {
            assert_eq!(
                scene_cursor_icon(false, None, Some(scale)),
                egui::CursorIcon::None,
                "a scale knob swaps the system cursor for the drawn scale cursor"
            );
            assert_eq!(
                scene_cursor_icon(true, Some(scale), None),
                egui::CursorIcon::None
            );
        }
        assert_eq!(
            scene_cursor_icon(false, None, Some(GizmoHandle::Move(Axis::X))),
            egui::CursorIcon::Default,
            "moving keeps the system pointer"
        );
    }

    /// Writes `gizmo-cursors.png`: the drawn rotate and scale cursors on a
    /// light and a dark background, for a visual check. Needs a GPU adapter.
    #[test]
    #[ignore = "needs a GPU adapter; writes a PNG for a visual check"]
    fn snapshot_gizmo_cursors() {
        let path = crate::ui_snapshot::snapshot_dir().join("gizmo-cursors.png");
        crate::ui_snapshot::render_png(&path, egui::vec2(160.0, 60.0), 4.0, |ui| {
            let rect = ui.max_rect();
            let (dark, light) = rect.split_left_right_at_fraction(0.5);
            ui.painter()
                .rect_filled(dark, 0.0, crate::theme::NIGHT.shell);
            ui.painter()
                .rect_filled(light, 0.0, egui::Color32::from_rgb(0x8C, 0xB3, 0x6E));
            for half in [dark, light] {
                let (left, right) = half.split_left_right_at_fraction(0.5);
                super::paint_rotate_cursor(ui.painter(), left.center());
                super::paint_scale_cursor(ui.painter(), right.center());
            }
        })
        .expect("snapshot");
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
    fn studio_brand_mark_displays_the_app_logo_image() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), super::paint_brand_mark);
        let logo_texture = output
            .textures_delta
            .set
            .iter()
            .find(|(_, deltas)| deltas.iter().any(|delta| delta.image.size() == [128, 128]))
            .map(|(id, _)| *id);
        let paints_logo = logo_texture.is_some_and(|id| {
            output.shapes.iter().any(|shape| match &shape.shape {
                egui::epaint::Shape::Mesh(mesh) => mesh.texture_id == id,
                egui::epaint::Shape::Rect(rect) => rect
                    .brush
                    .as_ref()
                    .is_some_and(|brush| brush.fill_texture_id == id),
                _ => false,
            })
        });
        output.textures_delta.clear();
        assert!(paints_logo);
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
                        if rect.fill == crate::theme::NIGHT.accent
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
                if rect.fill == crate::theme::NIGHT.panel && rect.rect.height() > 180.0 =>
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
    fn a_narrow_asset_panel_stacks_its_preview_under_the_tree() {
        assert!(super::preview_below_tree(250.0), "the default left dock");
        assert!(
            !super::preview_below_tree(600.0),
            "a wide panel keeps it beside"
        );
    }

    #[test]
    fn a_panel_resizes_from_the_edge_that_faces_the_rest_of_the_window() {
        use super::{ResizeEdge, resize_edge};
        let parent = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1000.0, 600.0));
        let tall = egui::Rangef::new(0.0, 600.0);
        let wide = egui::Rangef::new(0.0, 1000.0);
        let left = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(250.0, 600.0));
        let right = egui::Rect::from_min_max(egui::pos2(700.0, 0.0), egui::pos2(1000.0, 600.0));
        let bottom = egui::Rect::from_min_max(egui::pos2(0.0, 400.0), egui::pos2(1000.0, 600.0));
        assert_eq!(
            resize_edge(parent, left),
            Some(ResizeEdge::Vertical(250.0, tall))
        );
        assert_eq!(
            resize_edge(parent, right),
            Some(ResizeEdge::Vertical(700.0, tall))
        );
        assert_eq!(
            resize_edge(parent, bottom),
            Some(ResizeEdge::Horizontal(400.0, wide))
        );
        assert_eq!(
            resize_edge(parent, parent),
            None,
            "a panel filling the area has no free edge"
        );
    }

    #[test]
    fn panels_are_soft_cards_on_the_window_surface() {
        let frame = super::studio_panel_frame();
        assert_eq!(frame.fill, crate::theme::NIGHT.panel);
        assert_eq!(frame.corner_radius, egui::CornerRadius::same(10));
        assert_eq!(frame.stroke.width, 1.0);
        assert_eq!(frame.stroke.color, crate::theme::NIGHT.border);
        assert_eq!(frame.inner_margin, egui::Margin::ZERO);
        assert_eq!(frame.outer_margin, egui::Margin::same(2), "a 4-point gap");
    }

    #[test]
    fn srgb_surface_clear_matches_the_dark_shell_gap() {
        let clear = super::shell_clear_color(wgpu::TextureFormat::Bgra8UnormSrgb);
        let [r, g, b, _] = egui::Rgba::from(crate::theme::NIGHT.shell).to_array();
        assert!((clear.r - f64::from(r)).abs() < 0.000_01);
        assert!((clear.g - f64::from(g)).abs() < 0.000_01);
        assert!((clear.b - f64::from(b)).abs() < 0.000_01);
        assert_eq!(clear.a, 1.0);
    }

    #[test]
    fn linear_surface_clear_keeps_the_shell_rgb_values() {
        let clear = super::shell_clear_color(wgpu::TextureFormat::Bgra8Unorm);
        let shell = crate::theme::NIGHT.shell;
        assert!((clear.r - f64::from(shell.r()) / 255.0).abs() < f64::EPSILON);
        assert!((clear.g - f64::from(shell.g()) / 255.0).abs() < f64::EPSILON);
        assert!((clear.b - f64::from(shell.b()) / 255.0).abs() < f64::EPSILON);
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
                    if rect.fill == crate::theme::NIGHT.panel
                        && rect.stroke.color == crate::theme::NIGHT.border
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
            egui::epaint::Shape::Text(text) if text.galley.text() == "No messages yet." => {
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
