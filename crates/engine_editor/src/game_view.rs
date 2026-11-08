//! The game view: what the Scene View shows while a game plays inside it.
//!
//! The editor binary runs the game as its own process and uploads each
//! frame it receives into a texture ([`GameView::texture`]); this module
//! draws that texture letterboxed to the game's aspect ratio, tracks
//! whether the game has keyboard focus, and asks for a new render size
//! when the panel changes. The letterbox and cursor maths are pure and
//! unit-tested; the binary uses [`frame_position`] to turn window cursor
//! positions into game-frame pixels.

/// Largest preview, by area: 1920×1080. A bigger panel is shown scaled up.
pub const MAX_PREVIEW_PIXELS: f32 = 1920.0 * 1080.0;

/// Whether Studio's own cursor is grabbed for the playing game.
///
/// The game asks for a lock (mouse look); Studio honours it only while the
/// player has not pressed Esc, and every method returns the grab change to
/// apply to Studio's window (`Some(true)` grab and hide, `Some(false)`
/// release), or `None` when nothing changes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StudioCursorLock {
    /// What the game last asked for.
    pub game_wants: bool,
    /// Whether Studio's cursor is grabbed right now.
    pub held: bool,
    /// Esc released the lock; it stays released until the player clicks the
    /// game or the game turns its wish off and on again.
    pub escaped: bool,
    /// The click that took the mouse back is still down.
    pub click_swallowed: bool,
}

impl StudioCursorLock {
    fn set_held(&mut self, held: bool) -> Option<bool> {
        (self.held != held).then(|| {
            self.held = held;
            held
        })
    }

    /// The game asked to lock (or release) the mouse. While the game view
    /// is unfocused the wish is only recorded; the next click in the view
    /// or focus regain grabs the cursor.
    pub fn on_game_request(&mut self, locked: bool, view_focused: bool) -> Option<bool> {
        self.game_wants = locked;
        if !locked {
            self.escaped = false;
            return self.set_held(false);
        }
        if self.escaped || !view_focused {
            return None;
        }
        self.set_held(true)
    }

    /// Esc always frees the cursor; the game's wish stays until it changes.
    pub fn on_escape(&mut self) -> Option<bool> {
        self.escaped = self.game_wants;
        self.set_held(false)
    }

    /// A click in the game view locks again if the game still wants it.
    pub fn on_click_in_view(&mut self) -> Option<bool> {
        self.escaped = false;
        self.set_held(self.game_wants)
    }

    /// A mouse button went down or up in the game view. Returns the grab
    /// change and whether the game gets the event: the click that takes the
    /// mouse back is Studio's, so a shooter does not fire on it.
    pub fn on_mouse_button(&mut self, pressed: bool) -> (Option<bool>, bool) {
        if pressed {
            let change = self.on_click_in_view();
            self.click_swallowed = change == Some(true);
            (change, !self.click_swallowed)
        } else {
            (None, !std::mem::take(&mut self.click_swallowed))
        }
    }

    /// The window or game view regained focus: re-apply a lock released by
    /// losing focus, but not one released by Esc.
    pub fn on_regain_focus(&mut self) -> Option<bool> {
        if self.escaped {
            return None;
        }
        self.set_held(self.game_wants)
    }

    /// Whether raw mouse motion should reach the game: always while the
    /// lock is held; otherwise only over the view, and not after Esc freed
    /// a lock the game still wants (the free cursor must not spin the
    /// camera).
    pub fn forwards_motion(&self, cursor_inside: bool) -> bool {
        self.held || (cursor_inside && !self.game_wants)
    }

    /// The preview stopped or the game view lost focus: free the cursor.
    /// The wish is kept so a click re-applies it.
    pub fn on_stop_or_blur(&mut self) -> Option<bool> {
        self.set_held(false)
    }

    /// The preview ended: forget the game's wish too.
    pub fn reset(&mut self) -> Option<bool> {
        self.game_wants = false;
        self.escaped = false;
        self.click_swallowed = false;
        self.set_held(false)
    }
}

/// The texture holding the newest game frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameTexture {
    /// The egui handle of the uploaded frame.
    pub id: egui::TextureId,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

/// Game-view state shared by the shell (drawing, focus) and the editor
/// binary (frames, input, size).
#[derive(Debug, Default)]
pub struct GameView {
    /// The newest frame, once the game has sent one.
    pub texture: Option<GameTexture>,
    /// Whether keyboard and mouse go to the game.
    pub focused: bool,
    /// Where the frame was drawn last, in egui points.
    pub image_rect: Option<egui::Rect>,
    /// The render size the panel wants, in physical pixels; the binary
    /// sends it to the game and clears it.
    pub size_request: Option<(u32, u32)>,
    /// The size last asked for, so an unchanged panel sends nothing.
    pub requested: Option<(u32, u32)>,
    /// Why the last preview ended badly, shown over the Scene View until
    /// the next Play.
    pub error: Option<String>,
}

impl GameView {
    /// A window cursor position (physical pixels, with the window's
    /// `scale_factor`) as game-frame pixels, or `None` when the cursor is
    /// not over the drawn frame.
    pub fn frame_position_from_window(
        &self,
        x: f64,
        y: f64,
        scale_factor: f64,
    ) -> Option<(f64, f64)> {
        let image = self.image_rect?;
        let texture = self.texture?;
        let scale = scale_factor.max(f64::EPSILON);
        let point = egui::pos2((x / scale) as f32, (y / scale) as f32);
        frame_position(point, image, (texture.width, texture.height))
    }

    /// Forgets the last session: texture, focus and size (not the error).
    pub fn reset(&mut self) {
        self.texture = None;
        self.focused = false;
        self.image_rect = None;
        self.size_request = None;
        self.requested = None;
    }
}

/// The GPU texture game frames are uploaded into and egui displays.
/// Recreated when the frame size changes; the egui handle stays the same.
pub struct GameFrameTexture {
    texture: wgpu::Texture,
    id: egui::TextureId,
    size: (u32, u32),
}

impl GameFrameTexture {
    /// Uploads one frame of sRGB RGBA8 pixels (`width × height × 4`
    /// bytes), creating or resizing `slot`'s texture as needed, and returns
    /// what the game view should draw. `None` if `rgba` has the wrong size.
    pub fn upload(
        slot: &mut Option<Self>,
        gpu: &engine_renderer::GpuContext,
        shell: &mut crate::EditorShell,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<GameTexture> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return None;
        }
        let device = gpu.device();
        let stale = slot
            .as_ref()
            .is_none_or(|current| current.size != (width, height));
        if stale {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("game preview frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // egui reads native textures as gamma-encoded, which is
                // exactly what the game's sRGB frame bytes are.
                format: crate::viewport::VIEWPORT_TEXTURE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let id = match slot.as_ref() {
                Some(current) => {
                    shell.update_texture(device, current.id, &view);
                    current.id
                }
                None => shell.register_texture(device, &view),
            };
            *slot = Some(Self {
                texture,
                id,
                size: (width, height),
            });
        }
        let current = slot.as_ref()?;
        gpu.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &current.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Some(GameTexture {
            id: current.id,
            width,
            height,
        })
    }
}

/// The render size for a panel `area` points across: its physical pixels,
/// scaled down to at most [`MAX_PREVIEW_PIXELS`].
pub fn preview_size(area: egui::Vec2, pixels_per_point: f32) -> (u32, u32) {
    let pixels = area * pixels_per_point;
    let scale = (MAX_PREVIEW_PIXELS / (pixels.x * pixels.y).max(1.0))
        .sqrt()
        .min(1.0);
    (
        (pixels.x * scale).round().max(1.0) as u32,
        (pixels.y * scale).round().max(1.0) as u32,
    )
}

/// The largest rectangle with the frame's aspect ratio centred in `area`.
pub fn letterbox(area: egui::Rect, frame: (u32, u32)) -> egui::Rect {
    let (width, height) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let scale = (area.width() / width).min(area.height() / height);
    egui::Rect::from_center_size(area.center(), egui::vec2(width * scale, height * scale))
}

/// `point` (egui points) as a position in the frame's pixels, or `None`
/// outside the drawn `image`.
pub fn frame_position(
    point: egui::Pos2,
    image: egui::Rect,
    frame: (u32, u32),
) -> Option<(f64, f64)> {
    if !image.contains(point) || image.width() <= 0.0 || image.height() <= 0.0 {
        return None;
    }
    let x = (point.x - image.left()) / image.width() * frame.0 as f32;
    let y = (point.y - image.top()) / image.height() * frame.1 as f32;
    Some((f64::from(x), f64::from(y)))
}

/// Draws the game view in `ui`'s remaining space.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut crate::EditorState) {
    let (area, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, crate::theme::NIGHT.shell);

    // A new size once the panel settles (not while a splitter drags).
    if !ui.input(|input| input.pointer.any_down()) {
        let wanted = preview_size(area.size(), ui.ctx().pixels_per_point());
        let changed = state.game_view.requested.is_none_or(|current| {
            current.0.abs_diff(wanted.0) > 4 || current.1.abs_diff(wanted.1) > 4
        });
        if changed {
            state.game_view.requested = Some(wanted);
            state.game_view.size_request = Some(wanted);
        }
    }

    // Click the game to play it; click anywhere else to take focus back.
    if response.clicked() {
        state.game_view.focused = true;
    } else if ui.input(|input| input.pointer.any_pressed()) && !response.hovered() {
        state.game_view.focused = false;
    }

    let Some(texture) = state.game_view.texture else {
        state.game_view.image_rect = None;
        let message = if state.play.is_compiling() {
            "Building the game…"
        } else {
            "Starting the game…"
        };
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(15.0),
            crate::theme::NIGHT.muted,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
        return;
    };
    let image = letterbox(area, (texture.width, texture.height));
    state.game_view.image_rect = Some(image);
    painter.image(
        texture.id,
        image,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    let palette = crate::theme::NIGHT;
    let (width, color) = if state.game_view.focused {
        (2.0, palette.accent)
    } else {
        (1.0, palette.border)
    };
    painter.rect_stroke(
        image,
        0.0,
        egui::Stroke::new(width, color),
        egui::StrokeKind::Inside,
    );
    let hint = if state.play.is_paused() {
        Some("Paused")
    } else if !state.game_view.focused {
        Some("Click the game to play")
    } else {
        None
    };
    if let Some(hint) = hint {
        let at = image.center_top() + egui::vec2(0.0, 14.0);
        let galley = painter.layout_no_wrap(
            hint.to_owned(),
            egui::FontId::proportional(12.0),
            palette.text,
        );
        let badge = egui::Rect::from_center_size(
            at + egui::vec2(0.0, galley.size().y / 2.0),
            galley.size() + egui::vec2(16.0, 6.0),
        );
        painter.rect_filled(
            badge,
            badge.height() / 2.0,
            palette.raised.gamma_multiply(0.92),
        );
        painter.galley(badge.center() - galley.size() / 2.0, galley, palette.text);
    }
}

/// Draws the last preview's error as a banner across the top of `area`.
pub(crate) fn paint_error(ui: &egui::Ui, area: egui::Rect, message: &str) {
    let banner = egui::Rect::from_min_size(area.min, egui::vec2(area.width(), 28.0));
    let painter = ui.painter_at(area);
    let palette = crate::theme::NIGHT;
    painter.rect_filled(banner, 0.0, palette.shell);
    painter.rect_filled(banner, 0.0, palette.danger.gamma_multiply(0.18));
    painter.text(
        banner.left_center() + egui::vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("Play stopped: {message} — see the Output tab"),
        egui::FontId::proportional(13.0),
        palette.danger,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_matches_the_panel_but_stays_within_budget() {
        assert_eq!(preview_size(egui::vec2(640.0, 360.0), 2.0), (1280, 720));
        let (width, height) = preview_size(egui::vec2(1600.0, 900.0), 2.0);
        assert!((width * height) as f32 <= MAX_PREVIEW_PIXELS * 1.01);
        assert!(
            (width as f32 / height as f32 - 16.0 / 9.0).abs() < 0.01,
            "aspect kept"
        );
        assert_eq!(preview_size(egui::vec2(0.0, 0.0), 2.0), (1, 1));
    }

    #[test]
    fn the_frame_is_letterboxed_in_the_middle_of_the_panel() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 100.0));
        let image = letterbox(area, (200, 100));
        assert_eq!(image.size(), egui::vec2(200.0, 100.0), "height-limited");
        assert_eq!(image.center(), area.center());
        let tall = letterbox(area, (100, 400));
        assert_eq!(tall.size(), egui::vec2(25.0, 100.0));
    }

    #[test]
    fn cursor_positions_map_into_frame_pixels_through_the_letterbox() {
        let image = egui::Rect::from_min_size(egui::pos2(100.0, 0.0), egui::vec2(200.0, 100.0));
        let frame = (1280, 640);
        assert_eq!(
            frame_position(egui::pos2(100.0, 0.0), image, frame),
            Some((0.0, 0.0))
        );
        assert_eq!(
            frame_position(egui::pos2(200.0, 50.0), image, frame),
            Some((640.0, 320.0))
        );
        assert_eq!(
            frame_position(egui::pos2(50.0, 50.0), image, frame),
            None,
            "in the bars"
        );
    }

    #[test]
    fn click_that_takes_the_mouse_back_does_not_reach_the_game() {
        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, true);
        // While the lock is held, clicks are the game's.
        assert_eq!(lock.on_mouse_button(true), (None, true));
        assert_eq!(lock.on_mouse_button(false), (None, true));
        lock.on_escape();
        // The click after Esc re-locks; neither its press nor release is forwarded.
        assert_eq!(lock.on_mouse_button(true), (Some(true), false));
        assert_eq!(lock.on_mouse_button(false), (None, false));
        // The next click is the game's again.
        assert_eq!(lock.on_mouse_button(true), (None, true));
        assert_eq!(lock.on_mouse_button(false), (None, true));
    }

    #[test]
    fn clicks_reach_a_game_that_does_not_want_the_lock() {
        // A pause menu frees the mouse: its buttons must still get clicks.
        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, true);
        lock.on_game_request(false, true);
        assert_eq!(lock.on_mouse_button(true), (None, true));
        assert_eq!(lock.on_mouse_button(false), (None, true));
    }

    #[test]
    fn escape_releases_studio_cursor_lock() {
        let mut lock = StudioCursorLock::default();
        assert_eq!(lock.on_game_request(true, true), Some(true));
        assert_eq!(lock.on_escape(), Some(false));
        assert_eq!(lock.on_escape(), None);
        assert_eq!(lock.on_click_in_view(), Some(true));
        assert_eq!(lock.on_game_request(false, true), Some(false));
        assert_eq!(lock.on_click_in_view(), None);
        assert_eq!(lock.on_game_request(true, true), Some(true));
        assert_eq!(lock.on_stop_or_blur(), Some(false));
    }

    #[test]
    fn escape_stays_released_until_a_real_click_or_new_request() {
        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, true);
        lock.on_escape();
        assert_eq!(lock.on_stop_or_blur(), None);
        assert_eq!(lock.on_regain_focus(), None);
        assert_eq!(
            lock.on_game_request(true, true),
            None,
            "focus echo does not re-grab"
        );
        assert_eq!(lock.on_click_in_view(), Some(true));

        lock.on_escape();
        assert_eq!(lock.on_game_request(false, true), None);
        assert_eq!(
            lock.on_game_request(true, true),
            Some(true),
            "false then true re-locks"
        );

        lock.on_escape();
        assert_eq!(lock.reset(), None);
        assert_eq!(
            lock.on_game_request(true, true),
            Some(true),
            "reset clears escape"
        );
    }

    #[test]
    fn forwards_motion_only_when_it_cannot_spin_a_free_cursor() {
        let mut lock = StudioCursorLock::default();
        // Not held, inside, game does not want the lock.
        assert!(lock.forwards_motion(true));
        // Outside and not held.
        assert!(!lock.forwards_motion(false));
        // Held: forwarded wherever the cursor is.
        lock.on_game_request(true, true);
        assert!(lock.forwards_motion(false));
        assert!(lock.forwards_motion(true));
        // Esc: game still wants it, not held, cursor inside.
        lock.on_escape();
        assert!(!lock.forwards_motion(true));
        assert!(!lock.forwards_motion(false));
    }

    #[test]
    fn lock_request_while_unfocused_waits_for_focus() {
        let mut lock = StudioCursorLock::default();
        assert_eq!(lock.on_game_request(true, false), None);
        assert!(lock.game_wants);
        assert_eq!(lock.on_regain_focus(), Some(true));

        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, false);
        assert_eq!(lock.on_click_in_view(), Some(true));

        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, true);
        lock.on_escape();
        assert_eq!(lock.on_game_request(true, false), None);
        assert_eq!(lock.on_regain_focus(), None);
    }

    #[test]
    fn release_request_frees_held_lock_even_when_unfocused() {
        let mut lock = StudioCursorLock::default();
        assert_eq!(lock.on_game_request(true, true), Some(true));
        assert_eq!(lock.on_game_request(false, false), Some(false));
    }

    #[test]
    fn blur_without_escape_reapplies_on_regain() {
        let mut lock = StudioCursorLock::default();
        lock.on_game_request(true, true);
        assert_eq!(lock.on_stop_or_blur(), Some(false));
        assert_eq!(lock.on_regain_focus(), Some(true));
    }
}
