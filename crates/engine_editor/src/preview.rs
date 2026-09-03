//! Read-only previews of the selected asset, drawn under the asset
//! browser.
//!
//! Everything here reads the already-decoded data held by
//! [`crate::AssetImporter`] (textures, glTF, audio) — nothing is
//! instantiated into the scene and no importer runs. The heavy parts (a
//! GPU thumbnail, a waveform reduction) are computed once into
//! [`PreviewCache`] and reused until the selection or the import pass id
//! changes.
//!
//! The pure summary/reduction helpers ([`mesh_summary`],
//! [`audio_summary`], [`texture_summary`], [`waveform_bins`],
//! [`downscale_rgba`], [`fit_size`], [`human_bytes`]) are unit-tested;
//! the egui drawing in [`show`] is covered by the editor boot smoke, the
//! same split the other panels use.
//!
//! ## Not done here
//!
//! - No 3D-rendered mesh preview — that needs a per-mesh viewport
//!   pipeline (Runtime & build track). A glTF shows counts plus its
//!   first embedded image as a flat thumbnail.
//! - No audio playback — the waveform is a static min/max sketch.
//! - No standalone material / prefab preview — those file formats don't
//!   exist yet; glTF materials appear in the mesh summary.
//! - Only the selected row previews; no thumbnail grid or hover
//!   previews.

use std::path::PathBuf;

use engine_asset::{ImportedAudio, ImportedGltf, ImportedTexture};

use crate::assets::AssetEntry;
use crate::import::{AssetImporter, ImportOutcome, ImportedAsset};

/// Longest side, in pixels, of the CPU-downscaled thumbnail uploaded to
/// egui.
const THUMBNAIL_MAX_PX: u32 = 256;
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

/// Cached preview render state for the currently selected asset — a GPU
/// thumbnail and/or a reduced waveform, rebuilt when the selection or
/// the import pass id changes.
#[derive(Default)]
pub struct PreviewCache {
    key: Option<(PathBuf, u64)>,
    texture: Option<egui::TextureHandle>,
    /// Original (pre-downscale) thumbnail dimensions, for aspect ratio.
    source_dims: Option<(u32, u32)>,
    waveform: Vec<(f32, f32)>,
}

impl PreviewCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn clear(&mut self) {
        self.key = None;
        self.texture = None;
        self.source_dims = None;
        self.waveform.clear();
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
                // The first embedded image is normally the base-color map.
                if let Some(image) = gltf.images.first() {
                    self.load_thumbnail(ctx, key, &image.rgba8, image.width, image.height);
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
/// any) using the decoded data in `importer`.
pub fn show(
    ui: &mut egui::Ui,
    entry: Option<&AssetEntry>,
    importer: &AssetImporter,
    cache: &mut PreviewCache,
) {
    ui.separator();
    ui.strong("Preview");

    let Some(entry) = entry else {
        cache.clear();
        ui.weak("Select an asset.");
        return;
    };

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

    match imported {
        ImportedAsset::Texture(texture) => {
            let summary = texture_summary(texture);
            if let (Some(handle), Some((w, h))) = (&cache.texture, cache.source_dims) {
                ui.add(egui::Image::new((
                    handle.id(),
                    fit_size(w, h, PREVIEW_DISPLAY_MAX),
                )));
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
                ui.add(egui::Image::new((
                    handle.id(),
                    fit_size(w, h, PREVIEW_DISPLAY_MAX),
                )));
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
        ImportedAsset::Audio(audio) => {
            paint_waveform(ui, &cache.waveform);
            let summary = audio_summary(audio);
            ui.weak(format!(
                "{} Hz \u{b7} {} ch \u{b7} {:.2}s",
                summary.sample_rate, summary.channels, summary.duration_secs
            ));
        }
    }
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
    use bytemuck::Zeroable;
    use engine_asset::ImportedMesh;
    use engine_renderer::Vertex;

    fn mesh(vertices: usize, indices: usize) -> ImportedMesh {
        ImportedMesh {
            name: None,
            vertices: vec![Vertex::zeroed(); vertices],
            indices: vec![0; indices],
            skin_weights: None,
            skeleton: None,
        }
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
