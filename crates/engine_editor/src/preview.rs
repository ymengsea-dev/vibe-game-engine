//! Read-only previews of the selected asset, drawn beside the asset browser.
#![cfg_attr(feature = "dock-shell", allow(dead_code))]
//!
//! Thumbnails and waveforms read decoded data held by
//! [`crate::AssetImporter`]; Play opens the selected audio file on demand.
//! Nothing is instantiated into the scene and no importer runs here. The
//! heavy parts (a thumbnail, a waveform reduction) are cached until
//! the selection or import pass changes.
//!
//! The pure summary/reduction helpers ([`mesh_summary`],
//! [`audio_summary`], [`texture_summary`], [`waveform_bins`],
//! [`downscale_rgba`], [`fit_size`], [`human_bytes`]) are unit-tested;
//! the egui drawing in [`show`] is covered by the editor boot smoke, the
//! same split the other panels use.
//!
//! ## Not done here
//!
//! - Mesh thumbnails are static, fixed-angle previews, not orbitable viewports.
//! - Audio playback is deliberately limited to Play/Stop (no seek or volume).
//! - No standalone material / prefab preview — those file formats don't
//!   exist yet; glTF materials appear in the mesh summary.
//! - Only the selected row previews; no thumbnail grid or hover
//!   previews.

use std::path::{Path, PathBuf};

use engine_asset::{ImportedAudio, ImportedGltf, ImportedTexture};
use engine_audio::{AudioContext, StreamingSound};
use glam::Vec3;
use kira::sound::{FromFileError, PlaybackState, streaming::StreamingSoundHandle};

use crate::assets::AssetEntry;
use crate::import::{AssetImporter, ImportOutcome, ImportedAsset};

/// Longest side, in pixels, of the CPU-downscaled thumbnail uploaded to
/// egui.
const THUMBNAIL_MAX_PX: u32 = 256;
const MODEL_THUMBNAIL_PX: usize = 160;
const MODEL_MAX_TRIANGLES: usize = 10_000;
const MODEL_MAX_PIXEL_VISITS: usize = 2_000_000;
/// Longest side, in points, the thumbnail is drawn at.
const PREVIEW_DISPLAY_MAX: f32 = 180.0;
/// How many min/max columns the waveform sketch reduces to.
const WAVEFORM_COLUMNS: usize = 240;

/// Counts extracted from an [`ImportedGltf`] for the mesh summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshSummary {
    /// Number of imported mesh primitives.
    pub meshes: usize,
    /// Total vertices across every primitive.
    pub vertices: usize,
    /// Total triangles across every primitive (`indices / 3`).
    pub triangles: usize,
    /// Number of materials.
    pub materials: usize,
    /// Number of embedded images.
    pub images: usize,
    /// Number of distinct skins/skeletons.
    pub skeletons: usize,
    /// Number of animation clips.
    pub animations: usize,
    /// Number of perspective cameras.
    pub cameras: usize,
}

/// Playback facts derived from an [`ImportedAudio`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSummary {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Source channel count (`0` is treated as mono for `frames`).
    pub channels: u16,
    /// Frames (samples per channel).
    pub frames: usize,
    /// Length in seconds, or `0.0` if the sample rate is `0`.
    pub duration_secs: f32,
}

/// Dimensions and mip info from an [`ImportedTexture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureSummary {
    /// Base width in pixels.
    pub width: u32,
    /// Base height in pixels.
    pub height: u32,
    /// Number of mip levels.
    pub mip_levels: usize,
    /// Byte length of the base (level 0) mip.
    pub level0_bytes: usize,
}

/// Cached thumbnail/waveform and selected audio playback. Image data is
/// rebuilt when the selection or import pass id changes.
#[derive(Default)]
pub struct PreviewCache {
    key: Option<(PathBuf, u64)>,
    texture: Option<egui::TextureHandle>,
    /// Original (pre-downscale) thumbnail dimensions, for aspect ratio.
    source_dims: Option<(u32, u32)>,
    waveform: Vec<(f32, f32)>,
    selected_path: Option<PathBuf>,
    audio: Option<AudioContext>,
    playing: Option<StreamingSoundHandle<FromFileError>>,
    audio_error: Option<String>,
}

impl PreviewCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn clear(&mut self) {
        self.stop_audio();
        self.key = None;
        self.texture = None;
        self.source_dims = None;
        self.waveform.clear();
    }

    pub(crate) fn sync_selection(&mut self, selected: Option<&Path>) {
        if self.selected_path.as_deref() != selected {
            self.stop_audio();
            self.audio_error = None;
            self.selected_path = selected.map(Path::to_path_buf);
        }
    }

    fn stop_audio(&mut self) {
        if let Some(mut handle) = self.playing.take() {
            handle.stop(kira::Tween::default());
        }
    }

    fn play_audio(&mut self, root: &Path, relative: &Path) {
        let result = (|| {
            let path = audio_file_path(root, relative)?;
            let sound = StreamingSound::from_file(path, false).map_err(|err| err.to_string())?;
            if self.audio.is_none() {
                self.audio = Some(AudioContext::new().map_err(|err| err.to_string())?);
            }
            self.audio
                .as_mut()
                .ok_or_else(|| "Audio output is unavailable".to_string())?
                .play_streaming(sound)
                .map_err(|err| err.to_string())
        })();
        match result {
            Ok(handle) => {
                self.playing = Some(handle);
                self.audio_error = None;
            }
            Err(error) => self.audio_error = Some(error),
        }
    }

    fn playing(&mut self) -> bool {
        if let Some(handle) = self.playing.as_mut() {
            if let Some(error) = handle.pop_error() {
                self.audio_error = Some(error.to_string());
                self.stop_audio();
                return false;
            }
            if handle.state() != PlaybackState::Stopped {
                return true;
            }
            self.playing = None;
        }
        false
    }

    fn rebuild(&mut self, ctx: &egui::Context, key: &(PathBuf, u64), imported: &ImportedAsset) {
        self.texture = None;
        self.source_dims = None;
        self.waveform.clear();

        match imported {
            ImportedAsset::Texture(texture) => {
                if let Some(level0) = texture.mip_levels.first() {
                    self.load_thumbnail(ctx, key, level0, texture.width, texture.height);
                }
            }
            ImportedAsset::Mesh(gltf) => {
                if let Some(rgba8) = model_thumbnail(gltf) {
                    self.load_thumbnail(
                        ctx,
                        key,
                        &rgba8,
                        MODEL_THUMBNAIL_PX as u32,
                        MODEL_THUMBNAIL_PX as u32,
                    );
                }
            }
            ImportedAsset::Audio(audio) => {
                self.waveform = waveform_bins(&audio.samples, audio.channels, WAVEFORM_COLUMNS);
            }
        }

        self.key = Some(key.clone());
    }

    fn load_thumbnail(
        &mut self,
        ctx: &egui::Context,
        key: &(PathBuf, u64),
        rgba8: &[u8],
        width: u32,
        height: u32,
    ) {
        if rgba8.len() != width as usize * height as usize * 4 {
            return;
        }
        let (small, small_w, small_h) = downscale_rgba(rgba8, width, height, THUMBNAIL_MAX_PX);
        let image =
            egui::ColorImage::from_rgba_unmultiplied([small_w as usize, small_h as usize], &small);
        let name = format!("vge_preview::{}", key.0.display());
        self.texture = Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR));
        self.source_dims = Some((width, height));
    }
}

/// Draws the preview for `entry` (the currently selected asset row, if
/// any) using decoded data in `importer`; Play reads from `asset_root`.
pub fn show(
    ui: &mut egui::Ui,
    entry: Option<&AssetEntry>,
    importer: &AssetImporter,
    cache: &mut PreviewCache,
    asset_root: &Path,
) {
    ui.separator();
    ui.label("Preview");

    let Some(entry) = entry else {
        cache.clear();
        ui.weak("No item selected");
        return;
    };
    cache.sync_selection(Some(&entry.relative_path));

    if is_audio_file(&entry.relative_path) {
        let imported = entry.id.and_then(|id| importer.get(id));
        let key = (entry.relative_path.clone(), importer.pass_id());
        if cache.key.as_ref() != Some(&key) {
            if let Some(imported @ ImportedAsset::Audio(_)) = imported {
                cache.rebuild(ui.ctx(), &key, imported);
            } else {
                cache.waveform.clear();
                cache.key = Some(key);
            }
        }
        ui.vertical_centered(|ui| {
            if cache.waveform.is_empty() {
                ui.weak("Waveform unavailable");
            } else {
                paint_waveform(ui, &cache.waveform);
            }
            if let Some(ImportedAsset::Audio(audio)) = imported {
                let summary = audio_summary(audio);
                ui.weak(format!(
                    "{} Hz · {} ch · {:.2}s",
                    summary.sample_rate, summary.channels, summary.duration_secs
                ));
            }
            let playing = cache.playing();
            if ui.button(if playing { "Stop" } else { "Play" }).clicked() {
                if playing {
                    cache.stop_audio();
                } else {
                    cache.play_audio(asset_root, &entry.relative_path);
                }
            }
            if let Some(error) = &cache.audio_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if cache.playing.is_some() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
        return;
    }

    if let Some(message) = failure_message(importer, entry) {
        cache.clear();
        ui.weak(format!("No preview \u{2014} {message}"));
        return;
    }

    let Some(imported) = entry.id.and_then(|id| importer.get(id)) else {
        cache.clear();
        ui.weak("No preview for this asset kind.");
        return;
    };

    let key = (entry.relative_path.clone(), importer.pass_id());
    if cache.key.as_ref() != Some(&key) {
        cache.rebuild(ui.ctx(), &key, imported);
    }

    ui.vertical_centered(|ui| match imported {
        ImportedAsset::Texture(texture) => {
            let summary = texture_summary(texture);
            if let (Some(handle), Some((w, h))) = (&cache.texture, cache.source_dims) {
                show_thumbnail(ui, handle, (w, h));
            }
            ui.weak(format!(
                "{}\u{d7}{} \u{b7} {} mips \u{b7} {}",
                summary.width,
                summary.height,
                summary.mip_levels,
                human_bytes(summary.level0_bytes)
            ));
        }
        ImportedAsset::Mesh(gltf) => {
            if let (Some(handle), Some((w, h))) = (&cache.texture, cache.source_dims) {
                show_thumbnail(ui, handle, (w, h));
            } else {
                ui.weak("No mesh geometry to preview");
            }
            let summary = mesh_summary(gltf);
            ui.weak(format!(
                "{} meshes \u{b7} {} verts \u{b7} {} tris",
                summary.meshes, summary.vertices, summary.triangles
            ));
            ui.weak(format!(
                "{} materials \u{b7} {} images \u{b7} {} skeletons",
                summary.materials, summary.images, summary.skeletons
            ));
            ui.weak(format!(
                "{} animations \u{b7} {} cameras",
                summary.animations, summary.cameras
            ));
        }
        ImportedAsset::Audio(_) => {}
    });
}

fn show_thumbnail(
    ui: &mut egui::Ui,
    handle: &egui::TextureHandle,
    (w, h): (u32, u32),
) -> egui::Response {
    let size = fit_size(w, h, PREVIEW_DISPLAY_MAX.min(ui.available_width()));
    ui.vertical_centered(|ui| ui.add(egui::Image::new((handle.id(), size))))
        .inner
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["wav", "ogg", "mp3", "flac"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind))
        })
}

fn audio_file_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|err| err.to_string())?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|err| err.to_string())?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err("Audio file must be inside project assets".to_string());
    }
    Ok(path)
}

/// The failure message from the last import pass for `entry`, if it
/// failed.
fn failure_message(importer: &AssetImporter, entry: &AssetEntry) -> Option<String> {
    importer.records().iter().find_map(|record| {
        if record.relative_path != entry.relative_path {
            return None;
        }
        match &record.outcome {
            ImportOutcome::Failed(message) => Some(message.clone()),
            _ => None,
        }
    })
}

fn paint_waveform(ui: &mut egui::Ui, bins: &[(f32, f32)]) {
    let width = ui.available_width().min(PREVIEW_DISPLAY_MAX * 1.6);
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, 48.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if bins.is_empty() {
        return;
    }

    let mid_y = rect.center().y;
    let half_h = rect.height() / 2.0;
    let column_width = rect.width() / bins.len() as f32;
    let color = egui::Color32::from_rgb(120, 170, 220);
    for (index, (low, high)) in bins.iter().enumerate() {
        let x = rect.left() + index as f32 * column_width + column_width / 2.0;
        let y_low = mid_y - low.clamp(-1.0, 1.0) * half_h;
        let y_high = mid_y - high.clamp(-1.0, 1.0) * half_h;
        painter.line_segment(
            [egui::pos2(x, y_low), egui::pos2(x, y_high)],
            egui::Stroke::new(1.0, color),
        );
    }
}

/// Renders one bounded, fixed-angle image from imported geometry. No viewport
/// or repaint timer is created; PreviewCache calls this only on a new key.
fn model_thumbnail(gltf: &ImportedGltf) -> Option<Vec<u8>> {
    let side = MODEL_THUMBNAIL_PX;
    let right = Vec3::new(1.0, 0.0, -1.0).normalize();
    let towards_camera = Vec3::new(1.0, 0.8, 1.0).normalize();
    let up = towards_camera.cross(right);
    let project = |position: [f32; 3]| {
        let point = Vec3::from(position);
        Vec3::new(point.dot(right), point.dot(up), point.dot(towards_camera))
    };

    // Sampling the bounds and triangles caps selection-time CPU work for large assets.
    // shortcut: node transforms are absent from ImportedGltf; use them when the importer retains them.
    let vertex_count = gltf
        .meshes
        .iter()
        .fold(0usize, |sum, mesh| sum.saturating_add(mesh.vertices.len()));
    let vertex_stride = vertex_count.div_ceil(200_000).max(1);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for mesh in &gltf.meshes {
        for vertex in mesh.vertices.iter().step_by(vertex_stride) {
            let point = project(vertex.position);
            if point.is_finite() {
                min = min.min(point);
                max = max.max(point);
            }
        }
    }
    let extent = (max - min).truncate();
    if !extent.is_finite() || extent.max_element() < 1e-6 {
        return None;
    }
    let scale = (side as f32 - 24.0) / extent.max_element();
    let center = (min + max) * 0.5;
    let screen = |position: [f32; 3]| {
        let p = project(position) - center;
        Vec3::new(
            side as f32 * 0.5 + p.x * scale,
            side as f32 * 0.5 - p.y * scale,
            p.z,
        )
    };

    let mut pixels = vec![0u8; side * side * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[26, 32, 42, 255]);
    }
    let mut depth = vec![f32::NEG_INFINITY; side * side];
    let triangle_count = gltf.meshes.iter().fold(0usize, |sum, mesh| {
        sum.saturating_add(mesh.indices.len() / 3)
    });
    let triangle_stride = triangle_count.div_ceil(MODEL_MAX_TRIANGLES).max(1);
    let mut triangles_seen = 0;
    let mut visits = 0;
    let mut drawn = false;
    let light = Vec3::new(0.4, 0.9, 0.7).normalize();

    'meshes: for mesh in &gltf.meshes {
        let material = mesh.material.and_then(|index| gltf.materials.get(index));
        let factor = material.map_or([0.55, 0.72, 0.9, 1.0], |m| m.base_color_factor);
        let image = material
            .and_then(|m| m.base_color_image)
            .and_then(|index| gltf.images.get(index))
            .filter(|image| {
                (image.width as usize)
                    .checked_mul(image.height as usize)
                    .and_then(|pixels| pixels.checked_mul(4))
                    == Some(image.rgba8.len())
                    && image.width > 0
                    && image.height > 0
            });
        for (triangle_index, indices) in mesh.indices.as_chunks::<3>().0.iter().enumerate() {
            if triangle_index % triangle_stride != 0 {
                continue;
            }
            if triangles_seen >= MODEL_MAX_TRIANGLES || visits >= MODEL_MAX_PIXEL_VISITS {
                break 'meshes;
            }
            triangles_seen += 1;
            let (Some(a), Some(b), Some(c)) = (
                mesh.vertices.get(indices[0] as usize),
                mesh.vertices.get(indices[1] as usize),
                mesh.vertices.get(indices[2] as usize),
            ) else {
                continue;
            };
            let points = [screen(a.position), screen(b.position), screen(c.position)];
            if !points.iter().all(|p| p.is_finite()) {
                continue;
            }
            let edge = |a: Vec3, b: Vec3, x: f32, y: f32| {
                (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
            };
            let area = edge(points[0], points[1], points[2].x, points[2].y);
            if !area.is_finite() || area.abs() < 1e-5 {
                continue;
            }
            let x0 = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
            let x1 = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
            let y0 = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
            let y1 = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
            if x1 < 0.0 || y1 < 0.0 || x0 >= side as f32 || y0 >= side as f32 {
                continue;
            }
            let x0 = x0.floor().clamp(0.0, (side - 1) as f32) as usize;
            let x1 = x1.ceil().clamp(0.0, (side - 1) as f32) as usize;
            let y0 = y0.floor().clamp(0.0, (side - 1) as f32) as usize;
            let y1 = y1.ceil().clamp(0.0, (side - 1) as f32) as usize;
            let normal = (Vec3::from(b.position) - Vec3::from(a.position))
                .cross(Vec3::from(c.position) - Vec3::from(a.position))
                .normalize_or_zero();
            let shade = if normal.is_finite() {
                0.35 + 0.65 * normal.dot(light).abs()
            } else {
                0.65
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    visits += 1;
                    if visits > MODEL_MAX_PIXEL_VISITS {
                        break 'meshes;
                    }
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let weights = [
                        edge(points[1], points[2], px, py) / area,
                        edge(points[2], points[0], px, py) / area,
                        edge(points[0], points[1], px, py) / area,
                    ];
                    if weights
                        .iter()
                        .any(|weight| !weight.is_finite() || *weight < 0.0)
                    {
                        continue;
                    }
                    let z = weights[0] * points[0].z
                        + weights[1] * points[1].z
                        + weights[2] * points[2].z;
                    let offset = y * side + x;
                    if !z.is_finite() || z <= depth[offset] {
                        continue;
                    }
                    let texel = image.map(|image| {
                        let u = weights[0] * a.uv[0] + weights[1] * b.uv[0] + weights[2] * c.uv[0];
                        let v = weights[0] * a.uv[1] + weights[1] * b.uv[1] + weights[2] * c.uv[1];
                        let tx = (u.rem_euclid(1.0) * image.width as f32) as usize;
                        let ty = (v.rem_euclid(1.0) * image.height as f32) as usize;
                        let start = (ty * image.width as usize + tx) * 4;
                        &image.rgba8[start..start + 4]
                    });
                    if texel.is_some_and(|rgba| rgba[3] < 128) {
                        continue;
                    }
                    for channel in 0..3 {
                        let base = texel.map_or(255.0, |rgba| rgba[channel] as f32);
                        pixels[offset * 4 + channel] =
                            (base * factor[channel].clamp(0.0, 1.0) * shade).clamp(0.0, 255.0)
                                as u8;
                    }
                    depth[offset] = z;
                    drawn = true;
                }
            }
        }
    }
    drawn.then_some(pixels)
}

/// Per-primitive and total counts for the glTF summary line.
pub fn mesh_summary(gltf: &ImportedGltf) -> MeshSummary {
    MeshSummary {
        meshes: gltf.meshes.len(),
        vertices: gltf.meshes.iter().map(|mesh| mesh.vertices.len()).sum(),
        triangles: gltf.meshes.iter().map(|mesh| mesh.indices.len() / 3).sum(),
        materials: gltf.materials.len(),
        images: gltf.images.len(),
        skeletons: gltf.skeletons.len(),
        animations: gltf.animations.len(),
        cameras: gltf.cameras.len(),
    }
}

/// Frame count and duration for an [`ImportedAudio`]. A `0` sample rate
/// yields a `0.0` duration rather than a division by zero; a `0` channel
/// count is treated as mono.
pub fn audio_summary(audio: &ImportedAudio) -> AudioSummary {
    let channels = audio.channels.max(1) as usize;
    let frames = audio.samples.len() / channels;
    let duration_secs = if audio.sample_rate == 0 {
        0.0
    } else {
        frames as f32 / audio.sample_rate as f32
    };
    AudioSummary {
        sample_rate: audio.sample_rate,
        channels: audio.channels,
        frames,
        duration_secs,
    }
}

/// Dimensions, mip count and base-level size of an [`ImportedTexture`].
pub fn texture_summary(texture: &ImportedTexture) -> TextureSummary {
    TextureSummary {
        width: texture.width,
        height: texture.height,
        mip_levels: texture.mip_levels.len(),
        level0_bytes: texture.mip_levels.first().map_or(0, Vec::len),
    }
}

/// Reduces interleaved `samples` (channel count `channels`) to at most
/// `columns` `(min, max)` pairs over channel 0 — a cheap waveform
/// sketch. Empty input or `columns == 0` yields an empty vec; `columns`
/// larger than the frame count is clamped.
pub fn waveform_bins(samples: &[f32], channels: u16, columns: usize) -> Vec<(f32, f32)> {
    let channels = channels.max(1) as usize;
    let frames = samples.len() / channels;
    if frames == 0 || columns == 0 {
        return Vec::new();
    }
    let columns = columns.min(frames);
    let per_column = frames.div_ceil(columns);

    let mut bins = Vec::with_capacity(columns);
    let mut start = 0;
    while start < frames {
        let end = (start + per_column).min(frames);
        let mut low = f32::INFINITY;
        let mut high = f32::NEG_INFINITY;
        for frame in start..end {
            let value = samples[frame * channels];
            low = low.min(value);
            high = high.max(value);
        }
        bins.push((low, high));
        start = end;
    }
    bins
}

/// Nearest-neighbour downscale of a tightly-packed RGBA8 buffer so its
/// longest side is at most `max` pixels. Returns an owned copy unchanged
/// when it already fits, when `max` is `0`, or when `src`'s length does
/// not match `width * height * 4`.
pub fn downscale_rgba(src: &[u8], width: u32, height: u32, max: u32) -> (Vec<u8>, u32, u32) {
    let degenerate =
        width == 0 || height == 0 || max == 0 || src.len() != width as usize * height as usize * 4;
    if degenerate || (width <= max && height <= max) {
        return (src.to_vec(), width, height);
    }

    let scale = f64::from(max) / f64::from(width.max(height));
    let dst_w = ((f64::from(width) * scale).round() as u32).max(1);
    let dst_h = ((f64::from(height) * scale).round() as u32).max(1);

    let mut dst = Vec::with_capacity(dst_w as usize * dst_h as usize * 4);
    for y in 0..dst_h {
        let src_y = (u64::from(y) * u64::from(height) / u64::from(dst_h)).min(u64::from(height) - 1)
            as usize;
        for x in 0..dst_w {
            let src_x = (u64::from(x) * u64::from(width) / u64::from(dst_w))
                .min(u64::from(width) - 1) as usize;
            let offset = (src_y * width as usize + src_x) * 4;
            dst.extend_from_slice(&src[offset..offset + 4]);
        }
    }
    (dst, dst_w, dst_h)
}

/// Aspect-preserving on-screen size for a `width`x`height` image, capped
/// so the longest side is `max` points and never enlarged.
pub fn fit_size(width: u32, height: u32, max: f32) -> egui::Vec2 {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let scale = (max / w.max(h)).min(1.0);
    egui::vec2(w * scale, h * scale)
}

/// A short human-readable byte count (`512 B`, `2.0 KiB`, `5.0 MiB`).
pub fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_preview_path_stays_inside_assets_root() {
        let root = std::env::temp_dir().join(format!("vge-preview-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("sample.wav"), b"audio").unwrap();
        assert_eq!(
            audio_file_path(&root, std::path::Path::new("sample.wav")).unwrap(),
            root.join("sample.wav").canonicalize().unwrap()
        );
        assert!(audio_file_path(&root, std::path::Path::new("../outside.wav")).is_err());
        assert!(audio_file_path(&root, std::path::Path::new("missing.wav")).is_err());
        std::fs::remove_file(root.join("sample.wav")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn compressed_audio_files_are_playable_without_a_waveform_import() {
        for name in ["music.ogg", "voice.mp3", "ambient.flac", "effect.wav"] {
            assert!(is_audio_file(std::path::Path::new(name)), "{name}");
        }
        assert!(!is_audio_file(std::path::Path::new("cover.png")));
    }

    #[test]
    fn invalid_audio_reports_an_error_without_opening_an_output_device() {
        let root = std::env::temp_dir().join(format!("vge-preview-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("bad.wav"), b"not a wave file").unwrap();
        let mut cache = PreviewCache::new();
        cache.play_audio(&root, Path::new("bad.wav"));
        assert!(cache.playing.is_none());
        assert!(cache.audio_error.is_some());
        std::fs::remove_file(root.join("bad.wav")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn thumbnail_is_centered_in_a_wide_preview() {
        let ctx = egui::Context::default();
        let image = egui::ColorImage::from_rgba_unmultiplied([20, 20], &vec![255; 20 * 20 * 4]);
        let texture = ctx.load_texture("center-test", image, egui::TextureOptions::LINEAR);
        let mut rect = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 150.0),
                )),
                ..Default::default()
            },
            |ui| {
                rect = Some(show_thumbnail(ui, &texture, (20, 20)).rect);
            },
        );
        output.textures_delta.clear();
        let rect = rect.unwrap();
        assert!((rect.center().x - 150.0).abs() < 3.0, "{rect:?}");
    }
    use bytemuck::Zeroable;
    use engine_asset::ImportedMesh;
    use engine_renderer::Vertex;

    fn mesh(vertices: usize, indices: usize) -> ImportedMesh {
        ImportedMesh {
            name: None,
            vertices: vec![Vertex::zeroed(); vertices],
            indices: vec![0; indices],
            material: None,
            skin_weights: None,
            skeleton: None,
        }
    }

    fn triangle() -> ImportedMesh {
        let mut triangle = mesh(3, 3);
        triangle.vertices[0].position = [-1.0, -1.0, 0.0];
        triangle.vertices[1].position = [1.0, -1.0, 0.0];
        triangle.vertices[2].position = [0.0, 1.0, 0.0];
        triangle.indices = vec![0, 1, 2];
        triangle
    }

    #[test]
    fn mesh_summary_counts_primitives_and_triangles() {
        let gltf = ImportedGltf {
            meshes: vec![mesh(3, 3), mesh(6, 6)],
            ..ImportedGltf::default()
        };
        let summary = mesh_summary(&gltf);
        assert_eq!(summary.meshes, 2);
        assert_eq!(summary.vertices, 9);
        assert_eq!(summary.triangles, 3); // 3/3 + 6/3
    }

    #[test]
    fn mesh_without_images_gets_a_geometry_thumbnail() {
        let gltf = ImportedGltf {
            meshes: vec![triangle()],
            ..ImportedGltf::default()
        };
        let mut cache = PreviewCache::new();
        cache.rebuild(
            &egui::Context::default(),
            &(PathBuf::from("model.gltf"), 1),
            &ImportedAsset::Mesh(Box::new(gltf)),
        );
        assert!(cache.texture.is_some());
    }

    #[test]
    fn model_thumbnail_depicts_geometry_instead_of_the_first_image() {
        let gltf = ImportedGltf {
            meshes: vec![triangle()],
            images: vec![engine_asset::ImportedImage {
                width: 1,
                height: 1,
                rgba8: vec![255, 0, 0, 255],
            }],
            ..ImportedGltf::default()
        };
        let image = model_thumbnail(&gltf).unwrap();
        assert_eq!(image.len(), MODEL_THUMBNAIL_PX * MODEL_THUMBNAIL_PX * 4);
        assert!(
            image
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > pixel[0])
        );
    }

    #[test]
    fn model_thumbnail_rejects_empty_and_malformed_geometry() {
        assert!(model_thumbnail(&ImportedGltf::default()).is_none());
        let mut bad_index = triangle();
        bad_index.indices = vec![0, 1, 99];
        assert!(
            model_thumbnail(&ImportedGltf {
                meshes: vec![bad_index],
                ..ImportedGltf::default()
            })
            .is_none()
        );
        let mut non_finite = triangle();
        non_finite.vertices[1].position[0] = f32::NAN;
        assert!(
            model_thumbnail(&ImportedGltf {
                meshes: vec![non_finite],
                ..ImportedGltf::default()
            })
            .is_none()
        );
    }

    #[test]
    fn model_thumbnail_bounds_work_for_large_triangle_lists() {
        let mut large = triangle();
        large.indices = [0, 1, 2].repeat(MODEL_MAX_TRIANGLES + 1);
        let gltf = ImportedGltf {
            meshes: vec![large],
            ..ImportedGltf::default()
        };
        assert_eq!(
            model_thumbnail(&gltf).map(|image| image.len()),
            Some(MODEL_THUMBNAIL_PX * MODEL_THUMBNAIL_PX * 4)
        );
    }

    #[test]
    fn audio_summary_computes_duration_and_guards_zero_rate() {
        let audio = ImportedAudio {
            sample_rate: 8_000,
            channels: 2,
            samples: vec![0.0; 8_000 * 2],
        };
        let summary = audio_summary(&audio);
        assert_eq!(summary.frames, 8_000);
        assert!((summary.duration_secs - 1.0).abs() < 1e-6);

        let broken = ImportedAudio {
            sample_rate: 0,
            channels: 1,
            samples: vec![0.0; 10],
        };
        assert_eq!(audio_summary(&broken).duration_secs, 0.0);
    }

    #[test]
    fn audio_summary_treats_zero_channels_as_mono() {
        let audio = ImportedAudio {
            sample_rate: 100,
            channels: 0,
            samples: vec![0.0; 50],
        };
        assert_eq!(audio_summary(&audio).frames, 50);
    }

    #[test]
    fn waveform_bins_finds_per_column_min_max() {
        let samples = vec![-1.0, -0.5, 0.0, 0.5, 1.0, 0.5, 0.0, -0.5];
        let bins = waveform_bins(&samples, 1, 2);
        assert_eq!(bins, vec![(-1.0, 0.5), (-0.5, 1.0)]);
    }

    #[test]
    fn waveform_bins_reads_channel_zero_only() {
        // Stereo, L = 1.0, R = -1.0 throughout.
        let samples = vec![1.0, -1.0, 1.0, -1.0];
        assert_eq!(waveform_bins(&samples, 2, 1), vec![(1.0, 1.0)]);
    }

    #[test]
    fn waveform_bins_handles_empty_and_overlong_columns() {
        assert!(waveform_bins(&[], 2, 100).is_empty());
        assert_eq!(waveform_bins(&[0.1, 0.2, 0.3], 1, 999).len(), 3);
        assert!(waveform_bins(&[0.1, 0.2], 1, 0).is_empty());
    }

    #[test]
    fn downscale_rgba_caps_the_long_side_and_keeps_aspect() {
        let src = vec![255u8; 8 * 4 * 4]; // 8x4 RGBA8
        let (out, w, h) = downscale_rgba(&src, 8, 4, 4);
        assert_eq!((w, h), (4, 2));
        assert_eq!(out.len(), 4 * 2 * 4);
    }

    #[test]
    fn downscale_rgba_passes_through_when_small_or_malformed() {
        let src = vec![1u8; 2 * 2 * 4];
        assert_eq!(downscale_rgba(&src, 2, 2, 64), (src.clone(), 2, 2));
        // Length does not match the claimed dimensions.
        assert_eq!(downscale_rgba(&src, 9, 9, 4), (src.clone(), 9, 9));
    }

    #[test]
    fn fit_size_never_upscales() {
        assert_eq!(fit_size(10, 10, 180.0), egui::vec2(10.0, 10.0));
        assert_eq!(fit_size(360, 180, 180.0), egui::vec2(180.0, 90.0));
    }

    #[test]
    fn texture_summary_reports_dims_and_mips() {
        let texture = ImportedTexture {
            width: 4,
            height: 2,
            mip_levels: vec![vec![0u8; 32], vec![0u8; 8], vec![0u8; 4]],
        };
        let summary = texture_summary(&texture);
        assert_eq!(
            (
                summary.width,
                summary.height,
                summary.mip_levels,
                summary.level0_bytes
            ),
            (4, 2, 3, 32)
        );
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
