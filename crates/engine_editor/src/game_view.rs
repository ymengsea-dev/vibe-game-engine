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
}
